use tender::model::ids::{EpochTimestamp, Namespace, SessionName};
use tender::model::state::{ExitReason, RunStatus};
use tender::session::{self, SessionRoot};

pub fn cmd_wait(name: &str, timeout: Option<u64>, namespace: &Namespace) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, namespace, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let deadline = timeout.map(|t| std::time::Instant::now() + std::time::Duration::from_secs(t));

    loop {
        let mut meta = session::read_meta(&session)?;

        // Reconciliation: non-terminal + lock not held -> sidecar crashed
        if !meta.status().is_terminal() && !session::is_locked(&session)? {
            meta.reconcile_sidecar_lost(EpochTimestamp::now())?;
            session::write_meta_atomic(&session, &meta)?;
            // Fall through to terminal check below
        }

        if meta.status().is_terminal() {
            let json = serde_json::to_string_pretty(&meta)?;
            println!("{json}");

            match meta.status() {
                RunStatus::Exited { how, .. } => match how {
                    ExitReason::ExitedOk => return Ok(()),
                    ExitReason::ExitedError { .. } => std::process::exit(42),
                    _ => return Ok(()), // Killed, KilledForced, TimedOut
                },
                RunStatus::SpawnFailed { .. } => std::process::exit(2),
                RunStatus::SidecarLost { .. } => std::process::exit(3),
                RunStatus::DependencyFailed { reason, .. } => {
                    use tender::model::dep_fail::DepFailReason;
                    match reason {
                        DepFailReason::Failed => std::process::exit(4),
                        DepFailReason::TimedOut => std::process::exit(124),
                        DepFailReason::Killed => std::process::exit(137),
                    }
                }
                _ => return Ok(()),
            }
        }

        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                anyhow::bail!("timeout waiting for session {name}");
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
