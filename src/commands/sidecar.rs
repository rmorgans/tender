use std::path::PathBuf;

use anyhow::Context;

pub fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    let ready_fd: std::os::unix::io::RawFd = std::env::var("TENDER_READY_FD")
        .context("TENDER_READY_FD not set")?
        .parse()
        .context("TENDER_READY_FD is not a valid fd")?;

    tender::sidecar::run(session_dir, ready_fd)
}
