use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod commands;

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
        } => commands::cmd_start(&name, cmd, stdin, replace, timeout),
        Commands::Push { name } => commands::cmd_push(&name),
        Commands::Status { name } => commands::cmd_status(&name),
        Commands::Kill { name, force } => commands::cmd_kill(&name, force),
        Commands::List => commands::cmd_list(),
        Commands::Log {
            name,
            tail,
            follow,
            grep,
            since,
            raw,
        } => commands::cmd_log(&name, tail, follow, grep, since, raw),
        Commands::Wait { name, timeout } => commands::cmd_wait(&name, timeout),
        Commands::Sidecar { session_dir } => commands::cmd_sidecar(session_dir),
    };

    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
