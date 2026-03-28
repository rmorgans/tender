use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tender::model::ids::Namespace;

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
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
        /// Enable stdin pipe for push command
        #[arg(long)]
        stdin: bool,
        /// Replace existing session (kill + restart)
        #[arg(long)]
        replace: bool,
        /// Kill child after N seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Working directory for the child process
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Environment variable override (KEY=VALUE)
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Send stdin to a running session's child process
    Push {
        /// Session name
        name: String,
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Show session status
    Status {
        /// Session name
        name: String,
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Kill a supervised run
    Kill {
        /// Session name
        name: String,
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
        /// Force kill (SIGKILL immediately, no grace period)
        #[arg(short, long)]
        force: bool,
    },
    /// List all sessions
    List {
        /// Namespace to filter (lists all namespaces if omitted)
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Query session output log
    Log {
        /// Session name
        name: String,
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
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
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
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

/// Resolve an optional namespace string into a `Namespace`, defaulting to "default".
fn resolve_namespace(namespace: Option<String>) -> anyhow::Result<Namespace> {
    namespace
        .map(|s| Namespace::new(&s))
        .transpose()
        .map(|opt| opt.unwrap_or_else(Namespace::default_namespace))
        .map_err(Into::into)
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Start {
            name,
            namespace,
            cmd,
            stdin,
            replace,
            timeout,
            cwd,
            env_vars,
        } => resolve_namespace(namespace).and_then(|ns| {
            commands::cmd_start(
                &name,
                cmd,
                stdin,
                replace,
                timeout,
                cwd.as_deref(),
                &env_vars,
                &ns,
            )
        }),
        Commands::Push { name, namespace } => {
            resolve_namespace(namespace).and_then(|ns| commands::cmd_push(&name, &ns))
        }
        Commands::Status { name, namespace } => {
            resolve_namespace(namespace).and_then(|ns| commands::cmd_status(&name, &ns))
        }
        Commands::Kill {
            name,
            namespace,
            force,
        } => resolve_namespace(namespace).and_then(|ns| commands::cmd_kill(&name, force, &ns)),
        Commands::List { namespace } => match namespace
            .map(|s| Namespace::new(&s).map_err(anyhow::Error::from))
            .transpose()
        {
            Ok(ns) => commands::cmd_list(ns.as_ref()),
            Err(e) => Err(e),
        },
        Commands::Log {
            name,
            namespace,
            tail,
            follow,
            grep,
            since,
            raw,
        } => resolve_namespace(namespace)
            .and_then(|ns| commands::cmd_log(&name, tail, follow, grep, since, raw, &ns)),
        Commands::Wait {
            name,
            namespace,
            timeout,
        } => resolve_namespace(namespace).and_then(|ns| commands::cmd_wait(&name, timeout, &ns)),
        Commands::Sidecar { session_dir } => commands::cmd_sidecar(session_dir),
    };

    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
