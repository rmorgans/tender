use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroI32;
use std::os::unix::io::RawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

use crate::model::ids::{EpochTimestamp, Generation, RunId, SessionName};
use crate::model::meta::Meta;
use crate::model::spec::{LaunchSpec, StdinMode};
use crate::model::state::ExitReason;
use crate::platform::unix as platform;
use crate::session::{self, LockGuard, SessionDir, SessionRoot};

/// Run the sidecar process. Called from the `_sidecar` subcommand.
///
/// Contract:
/// - Acquire session lock
/// - Read launch spec from session dir
/// - Spawn child process (write child_pid breadcrumb immediately)
/// - Write meta.json with Running state (or SpawnFailed)
/// - Send meta JSON snapshot over ready pipe (no race with disk state)
/// - Capture child stdout/stderr to output.log with timestamps
/// - Write terminal state when child exits
/// - Release lock and exit
pub fn run(session_dir: PathBuf, ready_fd: RawFd) -> anyhow::Result<()> {
    // Wrap the fd so we can track whether it's been consumed.
    // write_ready_signal takes ownership — Option prevents double-close.
    let mut ready = Some(ready_fd);

    let result = run_inner(&session_dir, &mut ready);

    if let Err(ref e) = result {
        // Only signal error if the fd hasn't been consumed yet
        if let Some(fd) = ready.take() {
            let _ = platform::write_ready_signal(fd, &format!("ERROR:{e}\n"));
        }
    }

    result
}

/// Build and spawn the child process with piped stdout/stderr and its own process group.
fn spawn_child(argv: &[String], stdin_piped: bool) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    if stdin_piped {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    // Make child its own process group leader so kill(-pgid) kills the whole tree
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()
}

/// Create the stdin FIFO and spawn a forwarding thread from FIFO to child stdin.
fn setup_stdin_forwarding(
    session_dir: &Path,
    child: &mut std::process::Child,
    stdin_errors: &Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<()> {
    let fifo_path = session_dir.join("stdin.pipe");
    platform::mkfifo(&fifo_path)?;

    // Take child stdin — forward fifo data into it
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("child stdin not piped"))?;

    // Spawn forwarding thread (detached — not joined).
    // Thread exits when child stdin breaks or fifo is removed.
    let fifo_clone = fifo_path.clone();
    let errors_clone = Arc::clone(stdin_errors);
    std::thread::spawn(move || forward_stdin(fifo_clone, child_stdin, errors_clone));

    Ok(())
}

/// Spawn a timeout thread that kills the child's process group after `timeout_s` seconds.
/// Returns the `timed_out` flag. The caller passes a `cancel` flag to prevent the kill
/// after the child exits naturally (PID reuse safety).
fn setup_timeout(
    child_pid: i32,
    timeout_s: u64,
    cancel: Arc<AtomicBool>,
) -> Arc<AtomicBool> {
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_clone = Arc::clone(&timed_out);
    std::thread::spawn(move || {
        // Sleep in small increments so we can check the cancel flag
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return; // Child exited before timeout — don't kill
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if cancel.load(Ordering::Relaxed) {
            return; // Final check after deadline
        }
        timed_out_clone.store(true, Ordering::Relaxed);
        // Kill the process GROUP (negative PID), matching normal kill semantics
        unsafe {
            libc::kill(-child_pid, libc::SIGKILL);
        }
    });
    timed_out
}

/// Collect capture errors and stdin forwarding errors into a warning list.
fn collect_warnings(
    session_dir: &Path,
    stdin_errors: &Arc<Mutex<Vec<String>>>,
) -> Vec<String> {
    let mut warnings = Vec::new();

    // Collect capture errors
    let capture_err_path = session_dir.join("capture_errors.log");
    if let Ok(errors) = std::fs::read_to_string(&capture_err_path) {
        for line in errors.lines() {
            if !line.is_empty() {
                warnings.push(format!("log capture: {line}"));
            }
        }
    }

    // Collect stdin forwarding errors
    if let Ok(errs) = stdin_errors.lock() {
        for e in errs.iter() {
            warnings.push(e.clone());
        }
    }

    warnings
}

fn run_inner(session_dir: &Path, ready: &mut Option<RawFd>) -> anyhow::Result<()> {
    // --- Setup: lock, read spec, create meta ---
    let sidecar_identity = platform::self_identity()?;

    let session_name_str = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid session directory"))?;
    let session_name = SessionName::new(session_name_str)?;

    let root = session_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session dir has no parent"))?;
    let session_root = SessionRoot::new(root.to_path_buf());
    let session = session::open_raw(&session_root, &session_name)?;

    let _lock = LockGuard::try_acquire(&session)?;

    // Read launch spec
    let spec_path = session_dir.join("launch_spec.json");
    let spec_json = std::fs::read_to_string(&spec_path)
        .context("failed to read launch_spec.json")?;
    let launch_spec: LaunchSpec = serde_json::from_str(&spec_json)
        .context("invalid launch_spec.json")?;
    let _ = std::fs::remove_file(&spec_path);

    let run_id = RunId::new();
    let generation = Generation::first();

    let mut meta = Meta::new_starting(
        session_name,
        run_id,
        generation,
        launch_spec,
        sidecar_identity,
        EpochTimestamp::now(),
    );

    // Re-set CLOEXEC on the ready fd before spawning the child.
    // The sidecar inherited this fd with CLOEXEC cleared (so it survived the sidecar's exec),
    // but the child must NOT hold the pipe open — otherwise the CLI's read_to_string blocks
    // until the child exits, defeating the readiness handshake.
    if let Some(fd) = ready.as_ref() {
        let ret = unsafe { libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        if ret == -1 {
            anyhow::bail!(
                "failed to set CLOEXEC on ready fd: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    // --- Spawn child (with SpawnFailed handling inline) ---
    let stdin_piped = meta.launch_spec().stdin_mode == StdinMode::Pipe;
    let mut child = match spawn_child(meta.launch_spec().argv(), stdin_piped) {
        Ok(c) => c,
        Err(_) => {
            meta.transition_spawn_failed(EpochTimestamp::now())?;
            session::write_meta_atomic(&session, &meta)?;
            signal_meta_snapshot(ready, &meta)?;
            return Ok(()); // Not an error — SpawnFailed is a valid terminal state
        }
    };

    // Write child PID breadcrumb immediately — before anything else.
    // If sidecar crashes after spawn but before meta write, the reconciler
    // can find and kill the orphaned child using this file.
    let child_pid = child.id();
    let _ = std::fs::write(session_dir.join("child_pid"), child_pid.to_string());

    // Get child identity
    let child_identity = match platform::process_identity(child_pid) {
        Ok(id) => id,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(session_dir.join("child_pid"));
            meta.transition_spawn_failed(EpochTimestamp::now())?;
            session::write_meta_atomic(&session, &meta)?;
            signal_meta_snapshot(ready, &meta)?;
            return Ok(());
        }
    };

    // --- Stdin forwarding (conditional) ---
    let stdin_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    if meta.launch_spec().stdin_mode == StdinMode::Pipe {
        setup_stdin_forwarding(session_dir, &mut child, &stdin_errors)?;
    }

    // --- Transition to Running + readiness signal ---
    meta.transition_running(child_identity)?;
    session::write_meta_atomic(&session, &meta)?;
    signal_meta_snapshot(ready, &meta)?;

    // --- Timeout setup ---
    let timeout_cancel = Arc::new(AtomicBool::new(false));
    let timed_out = if let Some(timeout_s) = meta.launch_spec().timeout_s {
        setup_timeout(child.id() as i32, timeout_s, Arc::clone(&timeout_cancel))
    } else {
        Arc::new(AtomicBool::new(false))
    };

    // --- Supervise ---
    let exit_reason = supervise(&session, &mut child)?;

    // --- Cancel timeout + collect warnings + determine exit reason ---
    timeout_cancel.store(true, Ordering::Relaxed);

    // Override reason if timeout fired (highest priority)
    let exit_reason = if timed_out.load(Ordering::Relaxed) {
        ExitReason::TimedOut
    } else {
        exit_reason
    };

    // Check for force-kill marker (lower priority than timeout)
    let kill_forced_path = session_dir.join("kill_forced");
    let exit_reason = if !matches!(exit_reason, ExitReason::TimedOut) && kill_forced_path.exists() {
        let _ = std::fs::remove_file(&kill_forced_path);
        ExitReason::KilledForced
    } else {
        let _ = std::fs::remove_file(&kill_forced_path);
        exit_reason
    };

    // Clean up stdin FIFO — forwarding thread will exit when open fails
    let _ = std::fs::remove_file(session_dir.join("stdin.pipe"));

    // Clean up breadcrumb — no longer needed, meta has the child identity
    let _ = std::fs::remove_file(session_dir.join("child_pid"));

    for warning in collect_warnings(session_dir, &stdin_errors) {
        meta.add_warning(warning);
    }

    // --- Write terminal state ---
    meta.transition_exited(exit_reason, EpochTimestamp::now())?;
    session::write_meta_atomic(&session, &meta)?;

    Ok(())
}

/// Forward data from the stdin FIFO to the child's stdin pipe.
/// Re-opens the FIFO after each writer disconnects to support multiple pushes.
/// Exits when: child stdin write fails (child exited) or FIFO open fails (removed).
fn forward_stdin(
    fifo_path: PathBuf,
    mut child_stdin: std::process::ChildStdin,
    errors: Arc<Mutex<Vec<String>>>,
) {
    use std::io::Read;
    let mut buf = [0u8; 8192];
    loop {
        // Open blocks until a writer connects
        let mut fifo = match File::open(&fifo_path) {
            Ok(f) => f,
            Err(e) => {
                if let Ok(mut errs) = errors.lock() {
                    errs.push(format!("stdin fifo open failed: {e}"));
                }
                return;
            }
        };
        loop {
            let n = match fifo.read(&mut buf) {
                Ok(0) => break, // writer disconnected
                Ok(n) => n,
                Err(e) => {
                    if let Ok(mut errs) = errors.lock() {
                        errs.push(format!("stdin fifo read failed: {e}"));
                    }
                    return;
                }
            };
            if child_stdin.write_all(&buf[..n]).is_err() {
                if let Ok(mut errs) = errors.lock() {
                    errs.push("stdin forwarding: child stdin closed".to_owned());
                }
                return;
            }
        }
    }
}

/// Send meta JSON over the readiness pipe. Consumes the fd.
/// The CLI reads this snapshot directly — no race with subsequent disk writes.
fn signal_meta_snapshot(ready: &mut Option<RawFd>, meta: &Meta) -> anyhow::Result<()> {
    let fd = ready
        .take()
        .ok_or_else(|| anyhow::anyhow!("readiness pipe already consumed"))?;
    let json = serde_json::to_string(meta)?;
    platform::write_ready_signal(fd, &format!("OK:{json}\n"))?;
    Ok(())
}

/// Supervise the child: capture stdout/stderr to output.log, wait for exit.
/// Returns the ExitReason when the child terminates.
fn supervise(session: &SessionDir, child: &mut std::process::Child) -> anyhow::Result<ExitReason> {
    let log_path = session.path().join("output.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log = Mutex::new(log_file);

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Spawn reader threads. Capture errors rather than silently discarding.
    let log_ref = &log;
    let (stdout_result, stderr_result) = std::thread::scope(|scope| {
        let stdout_handle = scope.spawn(move || capture_stream(stdout, 'O', log_ref));
        let stderr_handle = scope.spawn(move || capture_stream(stderr, 'E', log_ref));

        let stdout_r = stdout_handle
            .join()
            .unwrap_or_else(|_| Err("stdout capture thread panicked".into()));
        let stderr_r = stderr_handle
            .join()
            .unwrap_or_else(|_| Err("stderr capture thread panicked".into()));
        (stdout_r, stderr_r)
    });

    // Log capture failures to a file in the session dir.
    // Sidecar stderr goes to /dev/null so eprintln is useless.
    // Don't fail supervision — the child's exit status is still meaningful.
    let mut capture_errors = Vec::new();
    if let Err(e) = stdout_result {
        capture_errors.push(format!("stdout capture: {e}"));
    }
    if let Err(e) = stderr_result {
        capture_errors.push(format!("stderr capture: {e}"));
    }
    if !capture_errors.is_empty() {
        let err_path = session.path().join("capture_errors.log");
        let _ = std::fs::write(&err_path, capture_errors.join("\n"));
    }

    let status = child.wait()?;

    let reason = match status.code() {
        Some(0) => ExitReason::ExitedOk,
        Some(code) => {
            let code = NonZeroI32::new(code).expect("already excluded zero");
            ExitReason::ExitedError { code }
        }
        None => ExitReason::Killed,
    };

    Ok(reason)
}

/// Read lines from a stream and write to the shared log file.
/// Returns an error if log writing fails persistently.
fn capture_stream<R: std::io::Read>(stream: R, tag: char, log: &Mutex<File>) -> Result<(), String> {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // pipe closed
        };
        let ts = timestamp_micros();
        let formatted = format!("{ts} {tag} {line}\n");
        let mut f = log.lock().map_err(|e| format!("log mutex poisoned: {e}"))?;
        f.write_all(formatted.as_bytes())
            .map_err(|e| format!("log write failed: {e}"))?;
    }
    Ok(())
}

fn timestamp_micros() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let micros = duration.subsec_micros();
    format!("{secs}.{micros:06}")
}
