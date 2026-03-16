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
    /// List all sessions
    List,
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
        Commands::List => cmd_list(),
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
            // Sidecar died before signaling — clean up
            let _ = std::fs::remove_dir_all(session.path());
            anyhow::bail!("sidecar startup failed: {e}");
        }
    };

    if signal.starts_with("ERROR:") {
        let err_msg = signal.trim_start_matches("ERROR:").trim();
        let _ = std::fs::remove_dir_all(session.path());
        anyhow::bail!("sidecar failed: {err_msg}");
    }

    // Sidecar sends "OK:<json>\n" — parse the meta snapshot directly from pipe.
    // No disk re-read, no race with subsequent state transitions.
    let meta_json = signal
        .strip_prefix("OK:")
        .ok_or_else(|| anyhow::anyhow!("unexpected readiness signal: {signal}"))?
        .trim();

    // Pretty-print for human output (re-parse to format)
    let meta: serde_json::Value = serde_json::from_str(meta_json)?;
    println!("{}", serde_json::to_string_pretty(&meta)?);

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

fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    let ready_fd: std::os::unix::io::RawFd = std::env::var("TENDER_READY_FD")
        .map_err(|_| anyhow::anyhow!("TENDER_READY_FD not set"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("TENDER_READY_FD is not a valid fd"))?;

    tender::sidecar::run(session_dir, ready_fd)
}
