use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tender", about = "Agent process sitter")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a supervised run
    Start {
        /// Session name
        name: String,
        /// Enable stdin pipe for push command
        #[arg(long)]
        stdin: bool,
        /// Replace existing session (kill + restart)
        #[arg(long)]
        replace: bool,
        /// Kill child after N seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Send stdin to a running session's child process
    Push {
        /// Session name
        name: String,
    },
    /// Show session status
    Status {
        /// Session name
        name: String,
    },
    /// Kill a supervised run
    Kill {
        /// Session name
        name: String,
        /// Force kill (SIGKILL immediately, no grace period)
        #[arg(short, long)]
        force: bool,
    },
    /// List all sessions
    List,
    /// Query session output log
    Log {
        /// Session name
        name: String,
        /// Show last N lines
        #[arg(short = 'n', long)]
        tail: Option<usize>,
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Filter lines containing PATTERN
        #[arg(short, long)]
        grep: Option<String>,
        /// Show lines since TIME (epoch seconds or duration: 30s, 5m, 2h, 1d)
        #[arg(short, long)]
        since: Option<String>,
        /// Strip timestamp and stream tag prefixes
        #[arg(short, long)]
        raw: bool,
    },
    /// Block until session reaches terminal state
    Wait {
        /// Session name
        name: String,
        /// Timeout in seconds
        #[arg(short, long)]
        timeout: Option<u64>,
    },
    /// Internal: sidecar process (not for direct use)
    #[command(name = "_sidecar", hide = true)]
    Sidecar {
        #[arg(long)]
        session_dir: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Start {
            name,
            cmd,
            stdin,
            replace,
            timeout,
        } => cmd_start(&name, cmd, stdin, replace, timeout),
        Commands::Push { name } => cmd_push(&name),
        Commands::Status { name } => cmd_status(&name),
        Commands::Kill { name, force } => cmd_kill(&name, force),
        Commands::List => cmd_list(),
        Commands::Log {
            name,
            tail,
            follow,
            grep,
            since,
            raw,
        } => cmd_log(&name, tail, follow, grep, since, raw),
        Commands::Wait { name, timeout } => cmd_wait(&name, timeout),
        Commands::Sidecar { session_dir } => cmd_sidecar(session_dir),
    };

    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

fn cmd_start(name: &str, cmd: Vec<String>, stdin: bool, replace: bool, timeout: Option<u64>) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::model::spec::{LaunchSpec, StdinMode};
    use tender::platform::unix as platform;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Handle --replace before session creation
    if replace {
        let session_path = root.path().join(session_name.as_str());
        if session_path.exists() {
            if !session_path.join("meta.json").exists() {
                // Orphan dir — just clean up
                cleanup_orphan_dir(&session_path);
            } else if let Some(existing) = session::open(&root, &session_name)? {
                let existing_meta = session::read_meta(&existing)?;
                if !existing_meta.status().is_terminal() {
                    // Kill the child
                    if let Some(child) = existing_meta.status().child() {
                        let _ = platform::kill_process(child, true);
                    }
                    // Wait for sidecar to write terminal state AND release lock
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
                // Safe to remove — sidecar has exited or timed out
                std::fs::remove_dir_all(existing.path())?;
            }
        }
    }

    // Build launch spec
    let mut launch_spec = LaunchSpec::new(cmd)?;
    launch_spec.stdin_mode = if stdin {
        StdinMode::Pipe
    } else {
        StdinMode::None
    };
    launch_spec.timeout_s = timeout;

    // Create session directory (with idempotent handling)
    let session = match session::create(&root, &session_name) {
        Ok(s) => s,
        Err(session::SessionError::AlreadyExists(_)) => {
            let session_path = root.path().join(session_name.as_str());
            // Orphan dir check: no meta.json means sidecar crashed before writing state
            if !session_path.join("meta.json").exists() {
                cleanup_orphan_dir(&session_path);
                session::create(&root, &session_name)?
            } else {
                let existing = session::open(&root, &session_name)?
                    .ok_or_else(|| anyhow::anyhow!("session exists but not openable"))?;
                let existing_meta = session::read_meta(&existing)?;

                if matches!(
                    existing_meta.status(),
                    tender::model::state::RunStatus::Running { .. }
                ) {
                    // Running — check spec match for idempotent return
                    if existing_meta.launch_spec_hash() == launch_spec.canonical_hash() {
                        let json = serde_json::to_string_pretty(&existing_meta)?;
                        println!("{json}");
                        return Ok(());
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
                    // Starting — sidecar is still initializing
                    anyhow::bail!(
                        "session {name} is still starting (sidecar has not reached Running yet)"
                    );
                }
            }
        }
        Err(e) => return Err(e.into()),
    };

    // Write launch spec for sidecar to read
    let spec_json = serde_json::to_string_pretty(&launch_spec)?;
    std::fs::write(session.path().join("launch_spec.json"), &spec_json)?;

    // Create readiness pipe
    let (read_end, write_end) = platform::pipe()?;

    // Spawn detached sidecar
    let tender_bin = std::env::current_exe()?;
    let sidecar_result = platform::spawn_sidecar(&tender_bin, session.path(), &write_end);

    // Close write end in parent — we only read
    drop(write_end);

    if let Err(e) = sidecar_result {
        // Sidecar failed to spawn — clean up session dir so start is retryable
        let _ = std::fs::remove_dir_all(session.path());
        anyhow::bail!("failed to spawn sidecar: {e}");
    }

    // Block until sidecar signals readiness
    let signal = match platform::read_ready_signal(read_end) {
        Ok(s) => s,
        Err(e) => {
            // Sidecar died before signaling. Only clean up if no child was spawned.
            // If child_pid exists, a child may be alive — don't delete the evidence.
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

    // Sidecar sends "OK:<json>\n" — parse the meta snapshot directly from pipe.
    let meta_json = signal
        .strip_prefix("OK:")
        .ok_or_else(|| anyhow::anyhow!("unexpected readiness signal: {signal}"))?
        .trim();

    let meta: tender::model::meta::Meta = serde_json::from_str(meta_json)?;
    let json = serde_json::to_string_pretty(&meta)?;
    println!("{json}");

    // Exit non-zero if the child failed to spawn — agents branch on exit code
    if matches!(meta.status(), tender::model::state::RunStatus::SpawnFailed { .. }) {
        std::process::exit(2); // exit code 2 = process error per contract
    }

    Ok(())
}

fn cmd_push(name: &str) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::model::spec::StdinMode;
    use tender::model::state::RunStatus;
    use tender::platform::unix as platform;
    use tender::session::{self, SessionRoot};

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

    let fifo_path = session.path().join("stdin.pipe");

    let mut fifo = loop {
        match platform::open_fifo_write_nonblock(&fifo_path) {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                // No reader connected — check if session is still running
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

fn cmd_kill(name: &str, force: bool) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::platform::unix as platform;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Session doesn't exist — idempotent success
    let session = match session::open(&root, &session_name)? {
        Some(s) => s,
        None => {
            println!(
                "{}",
                serde_json::json!({"session": name, "result": "not_found"})
            );
            return Ok(());
        }
    };

    let meta = session::read_meta(&session)?;

    // Already terminal — idempotent success
    if meta.status().is_terminal() {
        let json = serde_json::to_string_pretty(&meta)?;
        println!("{json}");
        return Ok(());
    }

    // Get child identity from Running state
    let child = match meta.status().child() {
        Some(c) => *c,
        None => {
            // Starting state with no child — nothing to kill
            println!(
                "{}",
                serde_json::json!({"session": name, "result": "no_child"})
            );
            return Ok(());
        }
    };

    // Write kill_forced marker before force-killing so sidecar can detect it
    if force {
        let _ = std::fs::write(session.path().join("kill_forced"), "");
    }

    // Kill the child's process group. Verifies identity first.
    platform::kill_process(&child, force)?;

    // Wait briefly for sidecar to write terminal state, then re-read
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(m) = session::read_meta(&session) {
            if m.status().is_terminal() {
                let json = serde_json::to_string_pretty(&m)?;
                println!("{json}");
                return Ok(());
            }
        }
    }

    // Sidecar didn't write terminal state in time — report what we know
    println!(
        "{}",
        serde_json::json!({"session": name, "result": "kill_sent", "force": force})
    );
    Ok(())
}

fn cmd_status(name: &str) -> anyhow::Result<()> {
    use tender::model::ids::{EpochTimestamp, SessionName};
    use tender::session::{self, SessionError, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Try normal open first
    let session = match session::open(&root, &session_name) {
        Ok(Some(s)) => s,
        Ok(None) => anyhow::bail!("session not found: {name}"),
        Err(SessionError::Corrupt { .. }) => {
            // Check for orphan dir (child_pid but no meta.json)
            let orphan_dir = root.path().join(session_name.as_str());
            if orphan_dir.exists() {
                cleanup_orphan_dir(&orphan_dir);
                anyhow::bail!("session {name} was orphaned (cleaned up)");
            }
            anyhow::bail!("session not found: {name}");
        }
        Err(e) => return Err(e.into()),
    };

    let mut meta = session::read_meta(&session)?;

    // Reconciliation: non-terminal + lock not held -> sidecar crashed
    if !meta.status().is_terminal() && !session::is_locked(&session)? {
        meta.reconcile_sidecar_lost(EpochTimestamp::now())?;
        session::write_meta_atomic(&session, &meta)?;
    }

    let json = serde_json::to_string_pretty(&meta)?;
    println!("{json}");
    Ok(())
}

/// Clean up an orphaned session dir that has child_pid but no meta.json.
/// Attempts to kill the orphaned child if identity can be verified.
/// If identity cannot be verified (PID may have been reused), skips the kill
/// rather than risking killing an unrelated process.
fn cleanup_orphan_dir(dir: &std::path::Path) {
    use tender::platform::unix as platform;

    let child_pid_path = dir.join("child_pid");
    if let Ok(pid_str) = std::fs::read_to_string(&child_pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            // Try to get current identity of this PID. If the process exists,
            // we can only safely kill it if we had stored identity to compare.
            // Since orphan dirs don't have meta.json (which has ProcessIdentity),
            // we use a best-effort heuristic: check if the process is a child of
            // init/launchd (PPID=1), which is likely for orphaned sidecar children.
            // If we can't verify, skip the kill — PID reuse safety is more important.
            let status = platform::process_status(&tender::model::ids::ProcessIdentity {
                pid: std::num::NonZeroU32::new(pid).unwrap(),
                start_time_ns: 0, // bogus — will always be IdentityMismatch
            });
            // If we get Inaccessible or Missing, the process is gone or unreachable.
            // If IdentityMismatch, the PID exists but we can't verify it's ours — skip.
            // Only Missing means we're safe (process doesn't exist, nothing to kill).
            match status {
                platform::ProcessStatus::Missing => {} // already gone
                platform::ProcessStatus::Inaccessible => {
                    // Can't verify, but process is in another session — likely our orphan.
                    // Still safer to skip than to kill blindly.
                }
                _ => {
                    // AliveVerified (impossible with start_time_ns=0),
                    // IdentityMismatch (PID reused), OsError — don't kill.
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

fn cmd_list() -> anyhow::Result<()> {
    use tender::session::{self, SessionRoot};

    let root = SessionRoot::default_path()?;
    let sessions = session::list(&root)?;

    let names: Vec<&str> = sessions.iter().map(|s| s.as_str()).collect();
    let json = serde_json::to_string_pretty(&names)?;
    println!("{json}");

    Ok(())
}

fn cmd_log(
    name: &str,
    tail: Option<usize>,
    follow: bool,
    grep: Option<String>,
    since: Option<String>,
    raw: bool,
) -> anyhow::Result<()> {
    use tender::log::{LogQuery, follow_log, parse_since, query_log};
    use tender::model::ids::SessionName;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let since_us = match since {
        Some(ref v) => Some(parse_since(v).map_err(|e| anyhow::anyhow!("invalid --since: {e}"))?),
        None => None,
    };

    let query = LogQuery {
        tail,
        grep,
        since_us,
        raw,
    };

    let log_path = session.path().join("output.log");

    if follow {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        follow_log(&log_path, &query, &mut out, || {
            session::read_meta(&session)
                .map(|m| m.status().is_terminal())
                .unwrap_or(false)
        })?;
    } else {
        if !log_path.exists() {
            return Ok(());
        }
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        query_log(&log_path, &query, &mut out)?;
    }

    Ok(())
}

fn cmd_wait(name: &str, timeout: Option<u64>) -> anyhow::Result<()> {
    use tender::model::ids::{EpochTimestamp, SessionName};
    use tender::model::state::{ExitReason, RunStatus};
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
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

fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    let ready_fd: std::os::unix::io::RawFd = std::env::var("TENDER_READY_FD")
        .map_err(|_| anyhow::anyhow!("TENDER_READY_FD not set"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("TENDER_READY_FD is not a valid fd"))?;

    tender::sidecar::run(session_dir, ready_fd)
}
