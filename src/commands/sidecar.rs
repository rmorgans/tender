use std::path::PathBuf;

use tender::platform::{Current, Platform};

pub fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    // On Windows, allocate a hidden console so children can receive
    // GenerateConsoleCtrlEvent for graceful stop.
    #[cfg(windows)]
    tender::platform::windows::prepare_sidecar_console();

    let ready_writer = Current::ready_writer_from_env()
        .map_err(|e| anyhow::anyhow!("[ready_writer_from_env] {e}"))?;
    tender::sidecar::run(session_dir, ready_writer)
}
