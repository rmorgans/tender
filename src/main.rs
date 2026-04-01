use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tender::model::ids::{Namespace, Source};

mod commands;

#[derive(Parser)]
#[command(name = "tender", about = "Agent process sitter")]
struct Cli {
    /// Route command through SSH to a remote host (e.g. user@box)
    #[arg(long, global = true)]
    host: Option<String>,

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
        /// Interactive pseudo-terminal mode
        #[arg(long)]
        pty: bool,
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
        /// Command to run after the child exits (repeatable)
        #[arg(long = "on-exit", value_name = "COMMAND")]
        on_exit: Vec<String>,
        /// Wait for session(s) to exit before starting (repeatable)
        #[arg(long = "after", value_name = "SESSION")]
        after: Vec<String>,
        /// Proceed even if dependency exits non-zero
        #[arg(long = "any-exit", requires = "after")]
        any_exit: bool,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Run a script as a supervised session
    Run {
        /// Script file to run
        script: PathBuf,
        /// Interpreter to use (default: bash, or direct if +x)
        #[arg(long)]
        shell: Option<String>,
        /// Return immediately after start (don't wait for exit)
        #[arg(long, conflicts_with = "foreground")]
        detach: bool,
        /// Force foreground mode (overrides #tender: detach directive)
        #[arg(long, conflicts_with = "detach")]
        foreground: bool,
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
        /// Command to run after the child exits (repeatable)
        #[arg(long = "on-exit", value_name = "COMMAND")]
        on_exit: Vec<String>,
        /// Wait for session(s) to exit before starting (repeatable)
        #[arg(long = "after", value_name = "SESSION")]
        after: Vec<String>,
        /// Proceed even if dependency exits non-zero
        #[arg(long = "any-exit", requires = "after")]
        any_exit: bool,
        /// Arguments to pass to the script
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
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
    /// Execute a command in a running shell session
    Exec {
        /// Session name
        name: String,
        /// Namespace for session grouping
        #[arg(long)]
        namespace: Option<String>,
        /// Timeout in seconds (client-side only)
        #[arg(long)]
        timeout: Option<u64>,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Watch session events as an NDJSON stream
    Watch {
        /// Namespace to filter (watches all namespaces if omitted)
        #[arg(long)]
        namespace: Option<String>,
        /// Emit run lifecycle events only
        #[arg(long)]
        events: bool,
        /// Emit log output events only
        #[arg(long)]
        logs: bool,
        /// Emit annotation events
        #[arg(long)]
        annotations: bool,
        /// Skip initial state snapshot, only emit new events
        #[arg(long = "from-now")]
        from_now: bool,
    },
    /// Run a command and record an annotation in the session's event log
    Wrap {
        /// Session name (defaults to TENDER_SESSION env var)
        #[arg(long)]
        session: Option<String>,
        /// Namespace (defaults to TENDER_NAMESPACE env var)
        #[arg(long)]
        namespace: Option<String>,
        /// Annotation source (e.g. "cmux.claude-hook")
        #[arg(long)]
        source: String,
        /// Event name (e.g. "pre-tool-use")
        #[arg(long)]
        event: String,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Delete terminal sessions older than a threshold
    Prune {
        /// Delete sessions ended more than DURATION ago (e.g. 7d, 24h, 30m)
        #[arg(long, value_parser = parse_duration, conflicts_with = "all")]
        older_than: Option<Duration>,
        /// Delete all terminal sessions regardless of age
        #[arg(long, conflicts_with = "older_than")]
        all: bool,
        /// Namespace to prune (prunes all namespaces if omitted)
        #[arg(long)]
        namespace: Option<String>,
        /// Show what would be deleted without deleting
        #[arg(long)]
        dry_run: bool,
    },
    /// Attach to a PTY session's terminal
    Attach {
        /// Session name
        name: String,
        /// Namespace
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Internal: sidecar process (not for direct use)
    #[command(name = "_sidecar", hide = true)]
    Sidecar {
        #[arg(long)]
        session_dir: PathBuf,
    },
}

impl Commands {
    /// Reconstruct CLI args for this command, suitable for remote invocation.
    fn remote_args(&self) -> Vec<String> {
        match self {
            Commands::Start {
                name, namespace, stdin, pty, replace, timeout, cwd, env_vars,
                on_exit, after, any_exit, cmd,
            } => {
                let mut args = vec!["start".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if *stdin { args.push("--stdin".to_string()); }
                if *pty { args.push("--pty".to_string()); }
                if *replace { args.push("--replace".to_string()); }
                if let Some(t) = timeout { args.extend(["--timeout".to_string(), t.to_string()]); }
                if let Some(c) = cwd { args.extend(["--cwd".to_string(), c.display().to_string()]); }
                for e in env_vars { args.extend(["--env".to_string(), e.clone()]); }
                for o in on_exit { args.extend(["--on-exit".to_string(), o.clone()]); }
                for a in after { args.extend(["--after".to_string(), a.clone()]); }
                if *any_exit { args.push("--any-exit".to_string()); }
                args.push("--".to_string());
                args.extend(cmd.iter().cloned());
                args
            }
            Commands::Status { name, namespace } => {
                let mut args = vec!["status".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                args
            }
            Commands::List { namespace } => {
                let mut args = vec!["list".to_string()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                args
            }
            Commands::Log { name, namespace, tail, follow, grep, since, raw } => {
                let mut args = vec!["log".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if let Some(n) = tail { args.extend(["--tail".to_string(), n.to_string()]); }
                if *follow { args.push("--follow".to_string()); }
                if let Some(g) = grep { args.extend(["--grep".to_string(), g.clone()]); }
                if let Some(s) = since { args.extend(["--since".to_string(), s.clone()]); }
                if *raw { args.push("--raw".to_string()); }
                args
            }
            Commands::Push { name, namespace } => {
                let mut args = vec!["push".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                args
            }
            Commands::Kill { name, namespace, force } => {
                let mut args = vec!["kill".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if *force { args.push("--force".to_string()); }
                args
            }
            Commands::Wait { name, namespace, timeout } => {
                let mut args = vec!["wait".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if let Some(t) = timeout { args.extend(["--timeout".to_string(), t.to_string()]); }
                args
            }
            Commands::Watch { namespace, events, logs, annotations, from_now } => {
                let mut args = vec!["watch".to_string()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if *events { args.push("--events".to_string()); }
                if *logs { args.push("--logs".to_string()); }
                if *annotations { args.push("--annotations".to_string()); }
                if *from_now { args.push("--from-now".to_string()); }
                args
            }
            Commands::Attach { name, namespace } => {
                let mut args = vec!["attach".to_string(), name.clone()];
                if let Some(ns) = namespace {
                    args.extend(["--namespace".to_string(), ns.clone()]);
                }
                args
            }
            _ => unreachable!("remote_args called on unsupported command"),
        }
    }

    /// Return the subcommand name string for allowlist checking.
    fn name(&self) -> &'static str {
        match self {
            Commands::Start { .. } => "start",
            Commands::Status { .. } => "status",
            Commands::List { .. } => "list",
            Commands::Log { .. } => "log",
            Commands::Push { .. } => "push",
            Commands::Kill { .. } => "kill",
            Commands::Wait { .. } => "wait",
            Commands::Watch { .. } => "watch",
            Commands::Attach { .. } => "attach",
            Commands::Run { .. } => "run",
            Commands::Exec { .. } => "exec",
            Commands::Wrap { .. } => "wrap",
            Commands::Prune { .. } => "prune",
            Commands::Sidecar { .. } => "_sidecar",
        }
    }
}

/// Parse a human-readable duration string (e.g. "7d", "24h", "30m").
fn parse_duration(s: &str) -> Result<Duration, humantime::DurationError> {
    humantime::parse_duration(s)
}

/// Resolve an optional namespace string into a `Namespace`, defaulting to "default".
/// Used by commands that always operate in exactly one namespace.
fn resolve_namespace(namespace: Option<String>) -> anyhow::Result<Namespace> {
    namespace
        .map(|s| Namespace::new(&s))
        .transpose()
        .map(|opt| opt.unwrap_or_else(Namespace::default_namespace))
        .map_err(Into::into)
}

/// Parse an optional namespace string without defaulting.
/// Returns `None` when no namespace was provided — meaning varies by command:
/// "all namespaces" (list/watch/prune) or "defer to directive/default" (run).
fn parse_optional_namespace(namespace: Option<String>) -> anyhow::Result<Option<Namespace>> {
    namespace
        .map(|s| Namespace::new(&s).map_err(anyhow::Error::from))
        .transpose()
}

fn main() {
    let cli = Cli::parse();

    // If --host is set, dispatch to SSH transport.
    if let Some(ref host) = cli.host {
        let cmd_name = cli.command.name();
        if !tender::ssh::is_remote_supported(cmd_name) {
            eprintln!("command '{cmd_name}' is not supported over SSH (--host)");
            std::process::exit(1);
        }
        let args = cli.command.remote_args();
        match tender::ssh::exec_ssh(host, &args) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    let result = match cli.command {
        Commands::Start {
            name,
            namespace,
            cmd,
            stdin,
            pty,
            replace,
            timeout,
            cwd,
            env_vars,
            on_exit,
            after,
            any_exit,
        } => resolve_namespace(namespace).and_then(|ns| {
            commands::cmd_start(
                &name,
                cmd,
                stdin,
                replace,
                timeout,
                cwd.as_deref(),
                &env_vars,
                &on_exit,
                &after,
                any_exit,
                &ns,
                pty,
            )
        }),
        Commands::Run {
            script,
            shell,
            detach,
            foreground,
            namespace,
            stdin,
            replace,
            timeout,
            cwd,
            env_vars,
            on_exit,
            after,
            any_exit,
            args,
        } => parse_optional_namespace(namespace).and_then(|ns| {
            commands::cmd_run(
                &script,
                args,
                shell,
                detach,
                foreground,
                ns.as_ref(),
                timeout,
                stdin,
                replace,
                cwd.as_deref(),
                &env_vars,
                &on_exit,
                &after,
                any_exit,
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
        Commands::List { namespace } => {
            parse_optional_namespace(namespace).and_then(|ns| commands::cmd_list(ns.as_ref()))
        }
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
        Commands::Exec {
            name,
            namespace,
            timeout,
            cmd,
        } => resolve_namespace(namespace)
            .and_then(|ns| commands::cmd_exec(&name, cmd, timeout, &ns)),
        Commands::Watch {
            namespace,
            events,
            logs,
            annotations,
            from_now,
        } => parse_optional_namespace(namespace)
            .and_then(|ns| commands::cmd_watch(ns.as_ref(), events, logs, annotations, from_now)),
        Commands::Prune {
            older_than,
            all,
            namespace,
            dry_run,
        } => parse_optional_namespace(namespace)
            .and_then(|ns| commands::cmd_prune(older_than, all, ns.as_ref(), dry_run)),
        Commands::Wrap {
            session,
            namespace,
            source,
            event,
            cmd,
        } => {
            let session = session
                .or_else(|| std::env::var("TENDER_SESSION").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("--session required (or set TENDER_SESSION env var)")
                });
            let namespace = namespace.or_else(|| std::env::var("TENDER_NAMESPACE").ok());
            let source = Source::new(&source).map_err(anyhow::Error::from);
            match (session, resolve_namespace(namespace), source) {
                (Ok(s), Ok(ns), Ok(src)) => commands::cmd_wrap(&s, &ns, &src, &event, cmd),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
            }
        }
        Commands::Attach { name, namespace } => {
            resolve_namespace(namespace).and_then(|ns| commands::cmd_attach(&name, &ns))
        }
        Commands::Sidecar { session_dir } => commands::cmd_sidecar(session_dir),
    };

    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
