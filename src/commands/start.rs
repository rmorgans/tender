use anyhow::Context;
use tender::model::ids::SessionName;
use tender::model::spec::{LaunchSpec, StdinMode};
use tender::platform::{Current, Platform};
use tender::session::{self, SessionRoot};

pub fn cmd_start(
    name: &str,
    cmd: Vec<String>,
    stdin: bool,
    replace: bool,
    timeout: Option<u64>,
    cwd: Option<&std::path::Path>,
    env_vars: &[String],
) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Handle --replace before session creation
    let next_generation = if replace {
        handle_replace(&root, &session_name)?
    } else {
        None
    };

    // Build launch spec
    let mut launch_spec = LaunchSpec::new(cmd)?;
    launch_spec.stdin_mode = if stdin {
        StdinMode::Pipe
    } else {
        StdinMode::None
    };
    launch_spec.timeout_s = timeout;
    launch_spec.cwd = cwd.map(|p| p.to_path_buf());
    for entry in env_vars {
        let (key, value) = entry
            .split_once('=')
            .with_context(|| format!("invalid --env format: expected KEY=VALUE, got: {entry}"))?;
        launch_spec.env.insert(key.to_string(), value.to_string());
    }

    // Create session directory (with idempotent handling)
    let session = match session::create(&root, &session_name) {
        Ok(s) => s,
        Err(session::SessionError::AlreadyExists(_)) => {
            match try_idempotent_start(&root, &session_name, &launch_spec)? {
                None => return Ok(()), // idempotent return printed
                Some(s) => s,          // orphan cleaned, session re-created
            }
        }
        Err(e) => return Err(e.into()),
    };

    // Write generation hint for sidecar to pick up
    if let Some(next_gen) = next_generation {
        let _ = std::fs::write(
            session.path().join("generation"),
            next_gen.as_u64().to_string(),
        );
    }

    spawn_and_wait_ready(&session, &launch_spec)
}

/// Handle `--replace`: kill any existing session and remove its directory.
/// Returns `Some(next_generation)` when an existing session was replaced,
/// `None` when no session existed or only an orphan was cleaned.
fn handle_replace(
    root: &SessionRoot,
    session_name: &SessionName,
) -> anyhow::Result<Option<tender::model::ids::Generation>> {
    let session_path = root.path().join(session_name.as_str());
    if session_path.exists() {
        if !session_path.join("meta.json").exists() {
            // Orphan dir -- just clean up, no generation to read
            super::status::cleanup_orphan_dir(&session_path);
        } else if let Some(existing) = session::open(root, session_name)? {
            let existing_meta = session::read_meta(&existing)?;
            let prev_generation = existing_meta.generation();

            if !existing_meta.status().is_terminal() {
                // Kill the child
                if let Some(child) = existing_meta.status().child() {
                    let _ = Current::kill_orphan(child, true);
                }
                // Wait for sidecar to write terminal state AND release lock
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                let name = session_name.as_str();
                loop {
                    if let Ok(m) = session::read_meta(&existing) {
                        if m.status().is_terminal()
                            && !session::is_locked(&existing).unwrap_or(true)
                        {
                            break; // Terminal + unlocked = safe to remove
                        }
                    }
                    if std::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "replace timed out: old sidecar for {name} did not exit within 10s"
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            // Safe to remove -- sidecar has exited or timed out
            std::fs::remove_dir_all(existing.path())?;
            return Ok(Some(prev_generation.next()));
        }
    }
    Ok(None)
}

/// Handle `AlreadyExists` when creating a session.
///
/// Returns:
/// - `Ok(None)` if idempotent return was already printed (caller should `return Ok(())`)
/// - `Ok(Some(session))` if orphan was cleaned and session was re-created
/// - `Err` for conflicts, terminal state, or Starting state
fn try_idempotent_start(
    root: &SessionRoot,
    session_name: &SessionName,
    launch_spec: &LaunchSpec,
) -> anyhow::Result<Option<session::SessionDir>> {
    let session_path = root.path().join(session_name.as_str());
    let name = session_name.as_str();

    // Orphan dir check: no meta.json means sidecar crashed before writing state
    if !session_path.join("meta.json").exists() {
        super::status::cleanup_orphan_dir(&session_path);
        let session = session::create(root, session_name)?;
        return Ok(Some(session));
    }

    let existing = session::open(root, session_name)?
        .ok_or_else(|| anyhow::anyhow!("session exists but not openable"))?;
    let existing_meta = session::read_meta(&existing)?;

    if matches!(
        existing_meta.status(),
        tender::model::state::RunStatus::Running { .. }
    ) {
        // Running -- check spec match for idempotent return
        if existing_meta.launch_spec_hash() == launch_spec.canonical_hash() {
            let json = serde_json::to_string_pretty(&existing_meta)?;
            println!("{json}");
            Ok(None)
        } else {
            anyhow::bail!(
                "session conflict: {name} is running with a different launch spec (use --replace to override)"
            );
        }
    } else if existing_meta.status().is_terminal() {
        anyhow::bail!(
            "session already exists in terminal state: {name} (use --replace to restart)"
        );
    } else {
        // Starting -- sidecar is still initializing
        anyhow::bail!("session {name} is still starting (sidecar has not reached Running yet)");
    }
}

/// Write launch spec, spawn sidecar, wait for readiness, and print meta JSON.
fn spawn_and_wait_ready(
    session: &session::SessionDir,
    launch_spec: &LaunchSpec,
) -> anyhow::Result<()> {
    // Write launch spec for sidecar to read
    let spec_json = serde_json::to_string_pretty(launch_spec)?;
    std::fs::write(session.path().join("launch_spec.json"), &spec_json)?;

    // Create readiness pipe
    let (read_end, write_end) = Current::ready_channel()?;

    // Spawn detached sidecar
    let tender_bin = std::env::current_exe()?;
    let sidecar_result = Current::spawn_sidecar(&tender_bin, session.path(), &write_end);

    // Close write end in parent -- we only read
    drop(write_end);

    if let Err(e) = sidecar_result {
        // Sidecar failed to spawn -- clean up session dir so start is retryable
        let _ = std::fs::remove_dir_all(session.path());
        anyhow::bail!("failed to spawn sidecar: {e}");
    }

    // Block until sidecar signals readiness
    let signal = match Current::read_ready_signal(read_end) {
        Ok(s) => s,
        Err(e) => {
            // Sidecar died before signaling. Only clean up if no child was spawned.
            // If child_pid exists, a child may be alive -- don't delete the evidence.
            if !session.path().join("child_pid").exists() {
                let _ = std::fs::remove_dir_all(session.path());
            }
            anyhow::bail!("sidecar startup failed: {e}");
        }
    };

    if signal.starts_with("ERROR:") {
        let err_msg = signal.trim_start_matches("ERROR:").trim();
        if !session.path().join("child_pid").exists() {
            let _ = std::fs::remove_dir_all(session.path());
        }
        anyhow::bail!("sidecar failed: {err_msg}");
    }

    // Sidecar sends "OK:<json>\n" -- parse the meta snapshot directly from pipe.
    let meta_json = signal
        .strip_prefix("OK:")
        .ok_or_else(|| anyhow::anyhow!("unexpected readiness signal: {signal}"))?
        .trim();

    let meta: tender::model::meta::Meta = serde_json::from_str(meta_json)?;
    let json = serde_json::to_string_pretty(&meta)?;
    println!("{json}");

    // Exit non-zero if the child failed to spawn -- agents branch on exit code
    if matches!(
        meta.status(),
        tender::model::state::RunStatus::SpawnFailed { .. }
    ) {
        std::process::exit(2); // exit code 2 = process error per contract
    }

    Ok(())
}
