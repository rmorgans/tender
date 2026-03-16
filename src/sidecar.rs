use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroI32;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::ids::{Generation, RunId, SessionName};
use crate::model::meta::Meta;
use crate::model::spec::LaunchSpec;
use crate::model::state::ExitReason;
use crate::platform::unix as platform;
use crate::session::{self, LockGuard, SessionDir, SessionRoot};

/// Run the sidecar process. Called from the `_sidecar` subcommand.
///
/// Contract:
/// - Acquire session lock
/// - Read launch spec from session dir
/// - Spawn child process
/// - Write meta.json with Running state (or SpawnFailed)
/// - Signal readiness via ready_fd
/// - Capture child stdout/stderr to output.log with timestamps
/// - Write terminal state when child exits
/// - Release lock and exit
pub fn run(session_dir: PathBuf, ready_fd: RawFd) -> anyhow::Result<()> {
    match run_inner(&session_dir, ready_fd) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Try to signal the error to the waiting CLI.
            let _ = platform::write_ready_signal(ready_fd, &format!("ERROR:{e}\n"));
            Err(e)
        }
    }
}

fn run_inner(session_dir: &Path, ready_fd: RawFd) -> anyhow::Result<()> {
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
        .map_err(|e| anyhow::anyhow!("failed to read launch_spec.json: {e}"))?;
    let launch_spec: LaunchSpec = serde_json::from_str(&spec_json)
        .map_err(|e| anyhow::anyhow!("invalid launch_spec.json: {e}"))?;
    let _ = std::fs::remove_file(&spec_path);

    let run_id = RunId::new();
    let generation = Generation::first();

    let mut meta = Meta::new_starting(
        session_name,
        run_id,
        generation,
        launch_spec,
        sidecar_identity,
        now_epoch_secs(),
    );

    // Spawn child
    let argv = meta.launch_spec().argv();
    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Child failed to spawn — write SpawnFailed, signal, exit
            meta.transition_spawn_failed(now_epoch_secs())?;
            session::write_meta_atomic(&session, &meta)?;
            platform::write_ready_signal(ready_fd, "OK\n")?;
            return Err(anyhow::anyhow!("child spawn failed: {e}"));
        }
    };

    // Get child identity
    let child_pid = child.id();
    let child_identity = match platform::process_identity(child_pid) {
        Ok(id) => id,
        Err(e) => {
            // Can't identify child — kill it, write SpawnFailed
            let _ = child.kill();
            let _ = child.wait();
            meta.transition_spawn_failed(now_epoch_secs())?;
            session::write_meta_atomic(&session, &meta)?;
            platform::write_ready_signal(ready_fd, "OK\n")?;
            return Err(anyhow::anyhow!("failed to get child identity: {e}"));
        }
    };

    // Transition to Running
    meta.transition_running(child_identity)?;
    session::write_meta_atomic(&session, &meta)?;

    // Signal readiness — CLI reads Running state
    platform::write_ready_signal(ready_fd, "OK\n")?;

    // Capture output and supervise
    let exit_reason = supervise(&session, &mut child)?;

    // Write terminal state
    meta.transition_exited(exit_reason, now_epoch_secs())?;
    session::write_meta_atomic(&session, &meta)?;

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

    // Take ownership of child's stdout/stderr pipes
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Spawn reader threads for concurrent capture without deadlock
    let log_ref = &log;
    std::thread::scope(|scope| {
        let stdout_handle = scope.spawn(move || capture_stream(stdout, 'O', log_ref));
        let stderr_handle = scope.spawn(move || capture_stream(stderr, 'E', log_ref));

        // Wait for both readers to finish (pipes close when child exits)
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();
    });

    // Wait for child to fully exit and get status
    let status = child.wait()?;

    let reason = match status.code() {
        Some(0) => ExitReason::ExitedOk,
        Some(code) => {
            // code is non-zero — safe to unwrap NonZeroI32
            let code = NonZeroI32::new(code).unwrap_or_else(|| {
                // Shouldn't happen since we checked Some(0) above, but be safe
                NonZeroI32::new(1).unwrap()
            });
            ExitReason::ExitedError { code }
        }
        None => {
            // Killed by signal
            ExitReason::Killed
        }
    };

    Ok(reason)
}

/// Read lines from a stream and write to the shared log file.
/// Each line is prefixed with `<epoch_us> <tag> `.
fn capture_stream<R: std::io::Read>(stream: R, tag: char, log: &Mutex<File>) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // pipe closed or read error
        };
        let ts = timestamp_micros();
        let formatted = format!("{ts} {tag} {line}\n");
        if let Ok(mut f) = log.lock() {
            let _ = f.write_all(formatted.as_bytes());
        }
    }
}

fn timestamp_micros() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let micros = duration.subsec_micros();
    format!("{secs}.{micros:06}")
}

fn now_epoch_secs() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", duration.as_secs())
}
