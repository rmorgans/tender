use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::Context;
use tender::directive::{self, Directives};
use tender::log::{LogQuery, follow_log};
use tender::model::ids::{EpochTimestamp, Namespace};
use tender::model::state::{ExitReason, RunStatus};
use tender::session;

pub fn cmd_run(
    script: &Path,
    args: Vec<String>,
    shell: Option<String>,
    detach: bool,
    foreground: bool,
    // CLI overrides for directives:
    namespace: Option<&Namespace>,
    timeout: Option<u64>,
    stdin: bool,
    replace: bool,
    cwd: Option<&Path>,
    env_vars: &[String],
    on_exit: &[String],
) -> anyhow::Result<()> {
    // --- Validate script file ---
    anyhow::ensure!(script.exists(), "script not found: {}", script.display());
    anyhow::ensure!(script.is_file(), "not a file: {}", script.display());

    // --- Parse directives ---
    let content = std::fs::read_to_string(script)
        .with_context(|| format!("failed to read script: {}", script.display()))?;
    let directives = directive::parse_directives(&content)
        .with_context(|| format!("failed to parse directives in {}", script.display()))?;

    // --- Resolve session name ---
    let session_name = if let Some(ref name) = directives.session {
        name.clone()
    } else {
        directive::derive_session_name(script).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    // --- Resolve shell and build argv ---
    let script_path = std::fs::canonicalize(script).unwrap_or_else(|_| script.to_path_buf());
    let script_str = script_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("script path is not valid UTF-8"))?;

    let cmd = resolve_shell_argv(shell.as_deref(), script_str, &script_path, args);

    // --- Merge directives with CLI overrides (CLI wins) ---
    let effective = merge_directives_with_cli(
        &directives,
        namespace,
        timeout,
        stdin,
        replace,
        cwd,
        env_vars,
        on_exit,
    );

    // --foreground overrides #tender: detach. --detach forces detach.
    // Without either CLI flag, honor the directive.
    let effective_detach = if foreground {
        false
    } else if detach {
        true
    } else {
        directives.detach
    };

    // --- Launch session ---
    let (meta, session) = super::start::launch_session(
        &session_name,
        cmd,
        effective.stdin,
        effective.replace,
        effective.timeout,
        effective.cwd.as_deref(),
        &effective.env_vars,
        &effective.on_exit,
        &effective.namespace,
    )?;

    // Check for spawn failure.
    if matches!(meta.status(), RunStatus::SpawnFailed { .. }) {
        eprintln!("error: child failed to spawn");
        std::process::exit(2);
    }

    // --- Detached mode: print JSON and return ---
    if effective_detach {
        let json = serde_json::to_string_pretty(&meta)?;
        println!("{json}");
        return Ok(());
    }

    // --- Foreground mode: follow log + wait for terminal state ---
    foreground_wait(&session)
}

/// Foreground mode: spawn a log-follow thread, poll for terminal state,
/// then propagate the child's actual exit code.
fn foreground_wait(session: &session::SessionDir) -> anyhow::Result<()> {
    let log_path = session.path().join("output.log");
    let meta_path = session.path().join("meta.json");

    // Shared stop flag: main thread sets it when terminal state is detected,
    // follow thread checks it on each poll.
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = Arc::clone(&stopped);

    // Spawn log-follow thread.
    let follow_handle = thread::spawn(move || {
        let query = LogQuery {
            tail: None,
            grep: None,
            since_us: Some(0), // Show all output from the beginning, don't seek to EOF.
            raw: true,
        };
        let should_stop = || stopped_clone.load(Ordering::Relaxed);
        let mut stdout = std::io::stdout().lock();
        let _ = follow_log(&log_path, &query, &mut stdout, should_stop);
    });

    // Main thread: poll meta.json for terminal state.
    let meta = loop {
        if let Ok(content) = std::fs::read_to_string(&meta_path) {
            if let Ok(mut meta) = serde_json::from_str::<tender::model::meta::Meta>(&content) {
                // Reconciliation: non-terminal + lock not held -> sidecar crashed
                if !meta.status().is_terminal()
                    && !session::is_locked(session).unwrap_or(true)
                    && meta.reconcile_sidecar_lost(EpochTimestamp::now()).is_ok()
                {
                    let _ = session::write_meta_atomic(session, &meta);
                }

                if meta.status().is_terminal() {
                    break meta;
                }
            }
        }

        thread::sleep(std::time::Duration::from_millis(200));
    };

    // Signal the follow thread to stop, then wait for it to drain.
    stopped.store(true, Ordering::Relaxed);
    let _ = follow_handle.join();

    // Propagate exit code.
    match meta.status() {
        RunStatus::Exited { how, .. } => match how {
            ExitReason::ExitedOk => Ok(()),
            ExitReason::ExitedError { code } => std::process::exit(code.get()),
            ExitReason::Killed | ExitReason::KilledForced => std::process::exit(137),
            ExitReason::TimedOut => std::process::exit(124),
        },
        RunStatus::SpawnFailed { .. } => std::process::exit(2),
        RunStatus::SidecarLost { .. } => std::process::exit(3),
        _ => Ok(()),
    }
}

/// Resolve the child command argv based on --shell flag and script executability.
fn resolve_shell_argv(
    shell: Option<&str>,
    script_str: &str,
    script_path: &Path,
    args: Vec<String>,
) -> Vec<String> {
    let mut cmd = Vec::new();

    if let Some(sh) = shell {
        cmd.push(sh.to_string());
        cmd.push(script_str.to_string());
    } else if is_executable(script_path) {
        cmd.push(script_str.to_string());
    } else {
        cmd.push("bash".to_string());
        cmd.push(script_str.to_string());
    }

    cmd.extend(args);
    cmd
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(_path: &Path) -> bool {
    false // Windows doesn't have +x; always fall back to shell.
}

/// Merged effective options (CLI overrides directives).
struct EffectiveOptions {
    namespace: Namespace,
    timeout: Option<u64>,
    stdin: bool,
    replace: bool,
    cwd: Option<PathBuf>,
    env_vars: Vec<String>,
    on_exit: Vec<String>,
}

fn merge_directives_with_cli(
    d: &Directives,
    cli_namespace: Option<&Namespace>,
    cli_timeout: Option<u64>,
    cli_stdin: bool,
    cli_replace: bool,
    cli_cwd: Option<&Path>,
    cli_env_vars: &[String],
    cli_on_exit: &[String],
) -> EffectiveOptions {
    // CLI namespace (pre-validated in main.rs) overrides directive namespace
    // (pre-validated in parse_directives). Fall back to default if neither set.
    let namespace = if let Some(ns) = cli_namespace {
        ns.clone()
    } else if let Some(ref ns_str) = d.namespace {
        Namespace::new(ns_str).expect("directive namespace was pre-validated")
    } else {
        Namespace::default_namespace()
    };

    let timeout = cli_timeout.or(d.timeout);
    let stdin = cli_stdin || d.stdin_pipe;
    let replace = cli_replace || d.replace;

    let cwd = cli_cwd
        .map(Path::to_path_buf)
        .or_else(|| d.cwd.as_ref().map(PathBuf::from));

    // Directive env entries + CLI env entries. CLI entries come after,
    // so they override in cmd_start's BTreeMap insertion.
    let mut env_vars: Vec<String> = d.env.values().cloned().collect();
    env_vars.extend(cli_env_vars.iter().cloned());

    // on-exit: if CLI specifies any, use CLI only (override). Otherwise use directives.
    let on_exit = if cli_on_exit.is_empty() {
        d.on_exit.clone()
    } else {
        cli_on_exit.to_vec()
    };

    EffectiveOptions {
        namespace,
        timeout,
        stdin,
        replace,
        cwd,
        env_vars,
        on_exit,
    }
}
