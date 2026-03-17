use tender::model::ids::SessionName;
use tender::model::spec::StdinMode;
use tender::model::state::RunStatus;
use tender::platform::{Current, Platform};
use tender::session::{self, SessionRoot};

pub fn cmd_push(name: &str) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;

    // Push requires Running state explicitly
    if !matches!(meta.status(), RunStatus::Running { .. }) {
        anyhow::bail!("session is not running");
    }

    if meta.launch_spec().stdin_mode != StdinMode::Pipe {
        anyhow::bail!("session was not started with --stdin");
    }

    let mut fifo = loop {
        match Current::open_stdin_writer(session.path()) {
            Ok(f) => break f,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                // No reader connected -- check if session is still running
                let current = session::read_meta(&session)?;
                if !matches!(current.status(), RunStatus::Running { .. }) {
                    anyhow::bail!("session exited before push could connect");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("failed to open stdin pipe: {e}"));
            }
        }
    };

    let mut stdin = std::io::stdin().lock();
    std::io::copy(&mut stdin, &mut fifo)?;

    Ok(())
}
