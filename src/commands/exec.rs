use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

use tender::exec_frame;
use tender::log::LogLine;
use tender::model::ids::{Namespace, SessionName};
use tender::model::spec::StdinMode;
use tender::model::state::RunStatus;
use tender::platform::{Current, Platform};
use tender::session::{self, SessionDir, SessionRoot};

/// Advisory flock on `session_dir/exec.lock`, non-blocking.
/// Ensures only one exec runs on a session at a time.
#[derive(Debug)]
pub struct ExecLock {
    _file: File,
}

#[cfg(unix)]
impl ExecLock {
    /// Try to acquire exec lock. Fails immediately if another exec holds it.
    pub fn try_acquire(session: &SessionDir) -> anyhow::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let lock_path = session.path().join("exec.lock");
        let file = File::create(&lock_path)?;

        // SAFETY: file is an open File, so as_raw_fd() returns a valid fd.
        // LOCK_EX | LOCK_NB is a valid flock operation (non-blocking exclusive).
        // flock may fail (EWOULDBLOCK) but won't cause UB.
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                anyhow::bail!("another exec is already running on this session");
            }
            return Err(err.into());
        }

        Ok(Self { _file: file })
    }
}

#[cfg(windows)]
impl ExecLock {
    /// Try to acquire exec lock. Fails immediately if another exec holds it.
    pub fn try_acquire(session: &SessionDir) -> anyhow::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
        };

        let lock_path = session.path().join("exec.lock");
        let file = File::create(&lock_path)?;

        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED =
            unsafe { std::mem::zeroed() };
        let ret = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ret == 0 {
            let err = std::io::Error::last_os_error();
            anyhow::bail!("another exec is already running on this session: {err}");
        }

        Ok(Self { _file: file })
    }
}

#[derive(serde::Serialize)]
struct ExecResult {
    session: String,
    stdout: String,
    stderr: String,
    exit_code: i32,
    cwd_after: String,
    timed_out: bool,
    truncated: bool,
}

pub fn cmd_exec(
    name: &str,
    cmd: Vec<String>,
    timeout: Option<u64>,
    namespace: &Namespace,
) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, namespace, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;

    if !matches!(meta.status(), RunStatus::Running { .. }) {
        anyhow::bail!("session is not running");
    }

    if meta.launch_spec().io_mode == tender::model::spec::IoMode::Pty {
        anyhow::bail!("exec is not supported on PTY sessions");
    }

    if meta.launch_spec().stdin_mode != StdinMode::Pipe {
        anyhow::bail!("session was not started with --stdin");
    }

    let _lock = ExecLock::try_acquire(&session)?;

    // Validate cmd is non-empty (clap enforces this, but belt-and-suspenders)
    if cmd.is_empty() {
        anyhow::bail!("no command specified");
    }

    let token = exec_frame::generate_token();
    let result = run_exec(&session, &meta, &cmd, &token, timeout)?;

    // If timed out, the in-shell command may still be running.
    // Hold the exec lock and drain until the sentinel arrives (or session dies)
    // to prevent a second exec from injecting into a busy shell.
    if result.timed_out {
        drain_until_sentinel(&session, &token);
    }

    // Write annotation event to output.log (bounded by MAX_LINE)
    {
        use tender::annotation;

        let run_id = meta.run_id().to_string();
        let hook_stdin = shell_words::join(&cmd);
        let log_path = session.path().join("output.log");
        // Try full payload first
        let payload = serde_json::json!({
            "source": "agent.exec",
            "event": "exec",
            "run_id": run_id,
            "data": {
                "hook_stdin": hook_stdin,
                "command": &cmd,
                "hook_stdout": &result.stdout,
                "hook_stderr": &result.stderr,
                "hook_exit_code": result.exit_code,
                "cwd_after": &result.cwd_after,
                "sentinel": format!("TENDER_EXEC_{token}"),
                "timed_out": result.timed_out,
                "truncated": result.truncated,
            }
        });

        if !annotation::write_annotation_line(&log_path, &payload)? {
            // Truncate and retry
            let trunc_stdout =
                annotation::truncate_string(&result.stdout, annotation::MAX_FIELD_BYTES);
            let trunc_stderr =
                annotation::truncate_string(&result.stderr, annotation::MAX_FIELD_BYTES);
            let payload = serde_json::json!({
                "source": "agent.exec",
                "event": "exec",
                "run_id": run_id,
                "data": {
                    "hook_stdin": hook_stdin,
                    "command": &cmd,
                    "hook_stdout": trunc_stdout,
                    "hook_stderr": trunc_stderr,
                    "hook_exit_code": result.exit_code,
                    "cwd_after": &result.cwd_after,
                    "sentinel": format!("TENDER_EXEC_{token}"),
                    "timed_out": result.timed_out,
                    "truncated": true,
                }
            });
            if !annotation::write_annotation_line(&log_path, &payload)? {
                eprintln!("tender exec: annotation too large even after truncation, dropping");
            }
        }
    }

    let json = serde_json::to_string_pretty(&result)?;
    println!("{json}");

    if result.timed_out {
        eprintln!("exec timed out — command may still be running in the shell");
        std::process::exit(124);
    }
    if result.exit_code != 0 {
        std::process::exit(result.exit_code);
    }

    Ok(())
}

fn run_exec(
    session: &SessionDir,
    meta: &tender::model::meta::Meta,
    cmd: &[String],
    token: &str,
    timeout: Option<u64>,
) -> anyhow::Result<ExecResult> {
    let session_name = meta.session().as_str().to_string();
    let deadline = timeout.map(|t| Instant::now() + Duration::from_secs(t));

    // 1. Capture log cursor
    let log_path = session.path().join("output.log");
    let cursor = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);

    // 2. Frame the command according to the session shell.
    let framed = match shell_kind(meta) {
        ShellKind::PowerShell => exec_frame::powershell_frame(cmd, token),
        ShellKind::Cmd => {
            anyhow::bail!("exec does not support cmd.exe sessions; use PowerShell or a POSIX shell")
        }
        ShellKind::Other => exec_frame::unix_frame(cmd, token),
    };

    // 3. Send through stdin transport (with retry on ConnectionRefused)
    let mut writer = loop {
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                anyhow::bail!("timeout connecting to stdin transport");
            }
        }
        match Current::open_stdin_writer(session.path()) {
            Ok(f) => break f,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                let current = session::read_meta(session)?;
                if !matches!(current.status(), RunStatus::Running { .. }) {
                    anyhow::bail!("session exited before exec could connect");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("failed to open stdin pipe: {e}"));
            }
        }
    };
    writer.write_all(framed.as_bytes())?;
    drop(writer);

    let mut stdout_lines: Vec<String> = Vec::new();
    let mut stderr_lines: Vec<String> = Vec::new();

    // Wait for log file to exist
    while !log_path.exists() {
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                return Ok(ExecResult {
                    session: session_name,
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    cwd_after: String::new(),
                    timed_out: true,
                    truncated: false,
                });
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let file = std::fs::File::open(&log_path)?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(cursor))?;

    let mut buf = String::new();
    loop {
        // Check timeout
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                return Ok(ExecResult {
                    session: session_name,
                    stdout: stdout_lines.join("\n"),
                    stderr: stderr_lines.join("\n"),
                    exit_code: -1,
                    cwd_after: String::new(),
                    timed_out: true,
                    truncated: false,
                });
            }
        }

        buf.clear();
        let bytes = reader.read_line(&mut buf)?;
        if bytes == 0 {
            // No data available — check session is still running
            let current = session::read_meta(session)?;
            if !matches!(current.status(), RunStatus::Running { .. }) {
                anyhow::bail!("session exited while waiting for exec result");
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
        let Some(parsed) = serde_json::from_str::<LogLine>(trimmed).ok() else {
            continue;
        };

        match parsed.tag.as_str() {
            "O" => {
                // Check if this is the sentinel line
                if let Some((exit_code, cwd)) = parsed
                    .content_text()
                    .and_then(|content| exec_frame::parse_sentinel(content, token))
                {
                    return Ok(ExecResult {
                        session: session_name,
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code,
                        cwd_after: cwd,
                        timed_out: false,
                        truncated: false,
                    });
                }
                if let Some(content) = parsed.content_text() {
                    stdout_lines.push(content.to_owned());
                }
            }
            "E" => {
                if let Some(content) = parsed.content_text() {
                    stderr_lines.push(content.to_owned());
                }
            }
            _ => {
                // Skip annotations and other tags
            }
        }
    }
}

/// After a timeout, drain output.log until the sentinel arrives or the session dies.
/// This holds the exec lock open to prevent a second exec from injecting into a
/// shell that is still busy with the timed-out command.
fn drain_until_sentinel(session: &SessionDir, token: &str) {
    let log_path = session.path().join("output.log");
    let Ok(file) = std::fs::File::open(&log_path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    // Seek to near the end — the sentinel is recent. Leave 64KB margin
    // in case the command produced output between the timeout and now.
    let len = reader.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = len.saturating_sub(65536);
    let _ = reader.seek(SeekFrom::Start(start));

    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => {
                // Check if session is still alive
                if let Ok(current) = session::read_meta(session) {
                    if !matches!(current.status(), RunStatus::Running { .. }) {
                        return; // Session died — nothing more to drain
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(_) => {
                let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
                if let Ok(parsed) = serde_json::from_str::<LogLine>(trimmed) {
                    if parsed.tag == "O"
                        && parsed
                            .content_text()
                            .and_then(|content| exec_frame::parse_sentinel(content, token))
                            .is_some()
                    {
                        return; // Sentinel found — command finished
                    }
                }
            }
            Err(_) => return, // IO error — give up
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellKind {
    PowerShell,
    Cmd,
    Other,
}

fn shell_kind(meta: &tender::model::meta::Meta) -> ShellKind {
    shell_kind_from_argv0(meta.launch_spec().argv().first().map(|s| s.as_str()))
}

fn shell_kind_from_argv0(argv0: Option<&str>) -> ShellKind {
    let Some(argv0) = argv0 else {
        return ShellKind::Other;
    };
    let lower = argv0.to_lowercase();
    if lower.contains("powershell") || lower.contains("pwsh") {
        ShellKind::PowerShell
    } else if lower.ends_with("cmd.exe") || lower == "cmd" || lower.ends_with("\\cmd") {
        ShellKind::Cmd
    } else {
        ShellKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{ShellKind, shell_kind_from_argv0};

    #[test]
    fn detects_powershell_shells() {
        assert_eq!(
            shell_kind_from_argv0(Some("powershell.exe")),
            ShellKind::PowerShell
        );
        assert_eq!(
            shell_kind_from_argv0(Some(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            ShellKind::PowerShell
        );
    }

    #[test]
    fn detects_cmd_shells() {
        assert_eq!(shell_kind_from_argv0(Some("cmd")), ShellKind::Cmd);
        assert_eq!(shell_kind_from_argv0(Some("cmd.exe")), ShellKind::Cmd);
        assert_eq!(
            shell_kind_from_argv0(Some(r"C:\Windows\System32\cmd.exe")),
            ShellKind::Cmd
        );
    }

    #[test]
    fn leaves_other_shells_alone() {
        assert_eq!(shell_kind_from_argv0(Some("bash")), ShellKind::Other);
        assert_eq!(shell_kind_from_argv0(Some("zsh")), ShellKind::Other);
        assert_eq!(shell_kind_from_argv0(None), ShellKind::Other);
    }
}
