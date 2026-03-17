use std::path::PathBuf;

use tender::platform::{Current, Platform};

pub fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    let ready_writer = Current::ready_writer_from_env()?;
    tender::sidecar::run(session_dir, ready_writer)
}
