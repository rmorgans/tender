use std::path::{Path, PathBuf};
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
    after: &[String],
    any_exit: bool,
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
        after,
        any_exit,
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
        &effective.after,
        effective.any_exit,
        &effective.namespace,
        false, // pty not supported via `run`
        Some(tender::model::spec::ExecTarget::None),
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

/// Foreground mode: follow log until terminal, then propagate exit code.
///
/// The follow thread checks meta.json for terminal state on each poll cycle.
/// When it sees terminal state, it drains remaining output and exits. The main
/// thread then reads the final meta.json to extract the exit code.
///
/// This avoids the race where a separate terminal-state detector signals stop
/// before the sidecar has flushed its last log lines.
fn foreground_wait(session: &session::SessionDir) -> anyhow::Result<()> {
    let log_path = session.path().join("output.log");
    let meta_path = session.path().join("meta.json");
    let meta_path_clone = meta_path.clone();

    // The follow thread handles both output streaming and terminal detection.
    // Its should_stop closure reads meta.json directly — same as cmd_log --follow.
    // This means the follow thread exits only after it has drained all output
    // written before the terminal state, avoiding the flush race.
    let follow_handle = thread::spawn(move || {
        let query = LogQuery {
            tail: None,
            since_us: Some(0), // Show all output from the beginning, don't seek to EOF.
            raw: true,
        };
        let should_stop = || {
            std::fs::read_to_string(&meta_path_clone)
                .ok()
                .and_then(|c| serde_json::from_str::<tender::model::meta::Meta>(&c).ok())
                .map(|m| m.status().is_terminal())
                .unwrap_or(false)
        };
        let mut stdout = std::io::stdout().lock();
        let _ = follow_log(&log_path, &query, &mut stdout, should_stop);
    });

    // Wait for the follow thread to finish (it exits when terminal + no more data).
    let _ = follow_handle.join();

    // Now read the final meta.json for exit code. The follow thread already
    // confirmed terminal state, so this read should succeed.
    let content = std::fs::read_to_string(&meta_path)?;
    let mut meta: tender::model::meta::Meta = serde_json::from_str(&content)?;

    // Reconciliation: if the sidecar crashed between the follow thread's last
    // check and now (unlikely but possible).
    if !meta.status().is_terminal() && !session::is_locked(session).unwrap_or(true) {
        if meta.reconcile_sidecar_lost(EpochTimestamp::now()).is_ok() {
            let _ = session::write_meta_atomic(session, &meta);
        }
    }

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
        RunStatus::DependencyFailed { reason, .. } => {
            use tender::model::dep_fail::DepFailReason;
            match reason {
                DepFailReason::Failed => std::process::exit(4),
                DepFailReason::TimedOut => std::process::exit(124),
                DepFailReason::Killed | DepFailReason::KilledForced => std::process::exit(137),
            }
        }
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
    after: Vec<String>,
    any_exit: bool,
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
    cli_after: &[String],
    cli_any_exit: bool,
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

    // --after and --any-exit: CLI-only, no directive equivalent
    let after = cli_after.to_vec();
    let any_exit = cli_any_exit;

    EffectiveOptions {
        namespace,
        timeout,
        stdin,
        replace,
        cwd,
        env_vars,
        on_exit,
        after,
        any_exit,
    }
}
