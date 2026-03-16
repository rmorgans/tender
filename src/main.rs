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
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
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
        Commands::Start { name, cmd } => cmd_start(&name, cmd),
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
        Commands::Sidecar { session_dir } => cmd_sidecar(session_dir),
    };

    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

fn cmd_start(name: &str, cmd: Vec<String>) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::model::spec::{LaunchSpec, StdinMode};
    use tender::platform::unix as platform;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Build launch spec
    let mut launch_spec = LaunchSpec::new(cmd)?;
    launch_spec.stdin_mode = StdinMode::None;

    // Create session directory
    let session = session::create(&root, &session_name)?;

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

    let meta: serde_json::Value = serde_json::from_str(meta_json)?;
    println!("{}", serde_json::to_string_pretty(&meta)?);

    // Exit non-zero if the child failed to spawn — agents branch on exit code
    if meta.get("status").and_then(|s| s.as_str()) == Some("SpawnFailed") {
        std::process::exit(2); // exit code 2 = process error per contract
    }

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
    use tender::model::ids::SessionName;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;
    let json = serde_json::to_string_pretty(&meta)?;
    println!("{json}");

    Ok(())
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

fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    let ready_fd: std::os::unix::io::RawFd = std::env::var("TENDER_READY_FD")
        .map_err(|_| anyhow::anyhow!("TENDER_READY_FD not set"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("TENDER_READY_FD is not a valid fd"))?;

    tender::sidecar::run(session_dir, ready_fd)
}
