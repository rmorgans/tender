use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};

use crate::model::ids::{Generation, RunId, SessionName};
use crate::model::meta::Meta;
use crate::model::spec::LaunchSpec;
use crate::platform::unix as platform;
use crate::session::{self, LockGuard, SessionRoot};

/// Run the sidecar process. Called from the `_sidecar` subcommand.
/// This is the only code that runs in the detached sidecar process.
///
/// Contract:
/// - Acquire session lock
/// - Read launch spec from session dir
/// - Write meta.json with Starting state
/// - Signal readiness via ready_fd
/// - (Slice 4+: spawn child, supervise, write terminal state)
/// - Write terminal state before exiting
/// - Release lock and exit
pub fn run(session_dir: PathBuf, ready_fd: RawFd) -> anyhow::Result<()> {
    // All errors before readiness must be reported via the pipe
    match run_inner(&session_dir, ready_fd) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Try to signal the error to the waiting CLI.
            // If this fails too, the CLI will see EOF and report a generic error.
            let _ = platform::write_ready_signal(ready_fd, &format!("ERROR:{e}\n"));
            Err(e)
        }
    }
}

fn run_inner(session_dir: &Path, ready_fd: RawFd) -> anyhow::Result<()> {
    // Get our own identity
    let sidecar_identity = platform::self_identity()?;

    // Derive session name from directory name
    let session_name_str = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid session directory"))?;
    let session_name = SessionName::new(session_name_str)?;

    // Build SessionRoot from the parent of session_dir
    let root = session_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session dir has no parent"))?;
    let session_root = SessionRoot::new(root.to_path_buf());

    // Open the session dir (must already exist with launch_spec.json)
    let session = session::open_raw(&session_root, &session_name)?;

    // Acquire exclusive lock — we are the sole owner of this session
    let _lock = LockGuard::try_acquire(&session)?;

    // Read launch spec written by CLI
    let spec_path = session_dir.join("launch_spec.json");
    let spec_json = std::fs::read_to_string(&spec_path)
        .map_err(|e| anyhow::anyhow!("failed to read launch_spec.json: {e}"))?;
    let launch_spec: LaunchSpec = serde_json::from_str(&spec_json)
        .map_err(|e| anyhow::anyhow!("invalid launch_spec.json: {e}"))?;

    // Clean up spec file — it's been consumed
    let _ = std::fs::remove_file(&spec_path);

    // Build meta
    let run_id = RunId::new();
    let generation = Generation::first(); // TODO: read previous generation from existing meta

    let mut meta = Meta::new_starting(
        session_name,
        run_id,
        generation,
        launch_spec,
        sidecar_identity,
        now_epoch_secs(),
    );

    // Slice 4+: spawn child here, transition to Running, then supervise.
    // For now, no child spawn — write SpawnFailed as the truthful terminal state.
    // The design requires: start only returns after durable Running or SpawnFailed.
    meta.transition_spawn_failed(now_epoch_secs())?;
    session::write_meta_atomic(&session, &meta)?;

    // Signal readiness — CLI is blocking on this.
    // Meta is already in terminal state, so the CLI reads truthful durable state.
    platform::write_ready_signal(ready_fd, "OK\n")?;

    // Lock is released when _lock is dropped.
    Ok(())
}

fn now_epoch_secs() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    format!("{secs}")
}
