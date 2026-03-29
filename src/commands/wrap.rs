use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tender::model::ids::{Namespace, SessionName, Source};
use tender::platform::{Current, Platform};
use tender::session::{self, SessionRoot};

/// Maximum annotation line length (timestamp + tag + json + newline).
/// Sized to stay within common local-FS single-write atomicity assumptions.
const MAX_ANNOTATION_LINE: usize = 4096;

/// Maximum size for individual payload fields before truncation.
const MAX_FIELD_BYTES: usize = 3000;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn cmd_wrap(
    session: &str,
    namespace: &Namespace,
    source: &Source,
    event: &str,
    cmd: Vec<String>,
) -> anyhow::Result<()> {
    let run_id = std::env::var("TENDER_RUN_ID").map_err(|_| {
        anyhow::anyhow!("TENDER_RUN_ID not set — wrap must run inside a tender-supervised process")
    })?;

    // Resolve session dir structurally, not from env
    let root = SessionRoot::default_path()?;
    let session_name = SessionName::new(session)?;
    let session_dir = session::open(&root, namespace, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {session}"))?;

    // Read all stdin
    let mut stdin_buf = Vec::new();
    io::stdin().read_to_end(&mut stdin_buf)?;

    // Install stop-notification handler
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    install_stop_handler();

    // Spawn child via Platform trait
    if cmd.is_empty() {
        anyhow::bail!("no command specified");
    }
    let env = BTreeMap::new();
    let mut child = Current::spawn_child(&cmd, true, None, &env)
        .map_err(|e| anyhow::anyhow!("failed to spawn '{}': {e}", cmd[0]))?;
    let kill_handle = Current::child_kill_handle(&child);

    // Pipe buffered stdin to child
    if let Some(mut child_stdin) = Current::child_stdin(&mut child) {
        let _ = child_stdin.write_all(&stdin_buf);
        // Drop closes the pipe — child sees EOF
    }

    // Wait for child, handling stop requests
    let output = wait_with_signal_handling(&mut child, &kill_handle);

    let (stdout_bytes, stderr_bytes, exit_code) = match output {
        Ok(out) => (out.stdout, out.stderr, out.status.code()),
        Err(e) => {
            eprintln!("tender wrap: child wait failed: {e}");
            (Vec::new(), Vec::new(), None)
        }
    };

    // Replay captured output to caller
    let _ = io::stdout().write_all(&stdout_bytes);
    let _ = io::stderr().write_all(&stderr_bytes);

    // Build and write annotation
    let payload = build_annotation_payload(
        source,
        event,
        &run_id,
        &stdin_buf,
        &stdout_bytes,
        &stderr_bytes,
        exit_code,
        &cmd,
    );

    if let Some(line) = payload {
        let log_path = session_dir.path().join("output.log");
        if let Err(e) = write_annotation_line(&log_path, &line) {
            eprintln!("tender wrap: failed to write annotation: {e}");
        }
    }

    // Exit with child's exit code
    std::process::exit(exit_code.unwrap_or(1));
}

fn build_annotation_payload(
    source: &Source,
    event: &str,
    run_id: &str,
    stdin_buf: &[u8],
    stdout_bytes: &[u8],
    stderr_bytes: &[u8],
    exit_code: Option<i32>,
    cmd: &[String],
) -> Option<String> {
    let hook_stdin = try_parse_json_or_string(stdin_buf);
    let hook_stdout = try_parse_json_or_string(stdout_bytes);
    let hook_stderr = String::from_utf8_lossy(stderr_bytes).into_owned();

    // Try full payload first
    let payload = serde_json::json!({
        "source": source.as_str(),
        "event": event,
        "run_id": run_id,
        "data": {
            "hook_stdin": hook_stdin,
            "hook_stdout": hook_stdout,
            "hook_stderr": hook_stderr,
            "hook_exit_code": exit_code,
            "command": cmd,
            "truncated": false,
        }
    });

    let ts = timestamp_micros();
    let json = serde_json::to_string(&payload).expect("JSON serialization cannot fail");
    let line = format!("{ts} A {json}\n");

    if line.len() <= MAX_ANNOTATION_LINE {
        return Some(line);
    }

    // Truncate fields to fit
    let truncated_stdin = truncate_field(&hook_stdin, MAX_FIELD_BYTES);
    let truncated_stdout = truncate_field(&hook_stdout, MAX_FIELD_BYTES);
    let truncated_stderr = truncate_string(&hook_stderr, MAX_FIELD_BYTES);

    let payload = serde_json::json!({
        "source": source.as_str(),
        "event": event,
        "run_id": run_id,
        "data": {
            "hook_stdin": truncated_stdin,
            "hook_stdout": truncated_stdout,
            "hook_stderr": truncated_stderr,
            "hook_exit_code": exit_code,
            "command": cmd,
            "truncated": true,
        }
    });

    let json = serde_json::to_string(&payload).expect("JSON serialization cannot fail");
    let line = format!("{ts} A {json}\n");

    if line.len() <= MAX_ANNOTATION_LINE {
        return Some(line);
    }

    // Still too large — drop all data fields
    let payload = serde_json::json!({
        "source": source.as_str(),
        "event": event,
        "run_id": run_id,
        "data": {
            "hook_stdin": serde_json::Value::Null,
            "hook_stdout": serde_json::Value::Null,
            "hook_stderr": "",
            "hook_exit_code": exit_code,
            "command": cmd,
            "truncated": true,
        }
    });

    let json = serde_json::to_string(&payload).expect("JSON serialization cannot fail");
    let line = format!("{ts} A {json}\n");

    if line.len() <= MAX_ANNOTATION_LINE {
        return Some(line);
    }

    // Shouldn't happen — envelope alone is small. Drop entirely.
    eprintln!("tender wrap: annotation too large even after truncation, dropping");
    None
}

fn try_parse_json_or_string(bytes: &[u8]) -> serde_json::Value {
    if bytes.is_empty() {
        return serde_json::Value::Null;
    }
    let s = String::from_utf8_lossy(bytes);
    serde_json::from_str(&s).unwrap_or_else(|_| serde_json::Value::String(s.into_owned()))
}

fn truncate_field(val: &serde_json::Value, max_bytes: usize) -> serde_json::Value {
    match val {
        serde_json::Value::String(s) => serde_json::Value::String(truncate_string(s, max_bytes)),
        other => {
            let s = serde_json::to_string(other).unwrap_or_default();
            if s.len() <= max_bytes {
                other.clone()
            } else {
                serde_json::Value::String(truncate_string(&s, max_bytes))
            }
        }
    }
}

fn truncate_string(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    // Truncate at char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn write_annotation_line(log_path: &std::path::Path, line: &str) -> io::Result<()> {
    debug_assert!(
        line.len() <= MAX_ANNOTATION_LINE,
        "annotation line exceeds size limit: {} bytes",
        line.len()
    );
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    file.write_all(line.as_bytes())?;
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

#[cfg(unix)]
fn install_stop_handler() {
    // SAFETY: signal() is async-signal-safe. The handler only sets an AtomicBool.
    // Using raw libc here because rustix does not wrap signal-handler registration.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            stop_signal_handler as *const () as libc::sighandler_t,
        );
    }
}

#[cfg(unix)]
extern "C" fn stop_signal_handler(_: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

#[cfg(windows)]
fn install_stop_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        STOP_REQUESTED.store(true, Ordering::SeqCst);
        1 // TRUE — handled
    }

    // SAFETY: handler is a valid extern "system" function. We pass TRUE (1)
    // to add the handler.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}

fn wait_with_signal_handling(
    child: &mut <Current as Platform>::SupervisedChild,
    kill_handle: &<Current as Platform>::ChildKillHandle,
) -> io::Result<std::process::Output> {
    // Collect stdout/stderr in threads so we don't deadlock
    let stdout = Current::child_stdout(child);
    let stderr = Current::child_stderr(child);

    let stdout_handle = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut r) = stdout {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    let stderr_handle = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut r) = stderr {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    // Uniform poll loop — Platform::kill_child handles graceful→force escalation
    let mut stop_forwarded = false;
    let status = loop {
        if let Some(status) = Current::child_try_wait(child)? {
            break status;
        }
        if STOP_REQUESTED.load(Ordering::SeqCst) && !stop_forwarded {
            let _ = Current::kill_child(kill_handle, false);
            stop_forwarded = true;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    Ok(std::process::Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}
