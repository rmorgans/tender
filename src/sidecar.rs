use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroI32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

use crate::model::dep_fail::DepFailReason;
use crate::model::ids::{EpochTimestamp, Generation, Namespace, RunId, SessionName};
use crate::model::meta::Meta;
use crate::model::spec::{LaunchSpec, StdinMode};
use crate::model::state::ExitReason;
use crate::platform::{Current, Platform};
use crate::session::{self, LockGuard, SessionDir, SessionRoot};

/// Type alias for the platform's ReadyWriter to avoid verbose turbofish.
type ReadyWriter = <Current as Platform>::ReadyWriter;

/// Run the sidecar process. Called from the `_sidecar` subcommand.
///
/// Contract:
/// - Acquire session lock
/// - Read launch spec from session dir
/// - Spawn child process (write child_pid breadcrumb immediately)
/// - Write meta.json with Running state (or SpawnFailed)
/// - Send meta JSON snapshot over ready pipe (no race with disk state)
/// - Capture child stdout/stderr to output.log with timestamps
/// - Write terminal state when child exits
/// - Release lock and exit
pub fn run(session_dir: PathBuf, ready_writer: ReadyWriter) -> anyhow::Result<()> {
    // Wrap so we can track whether it's been consumed.
    // write_ready_signal takes ownership -- Option prevents double-use.
    let mut ready = Some(ready_writer);

    let result = run_inner(&session_dir, &mut ready);

    if let Err(ref e) = result {
        // Only signal error if the file hasn't been consumed yet
        if let Some(file) = ready.take() {
            let _ = Current::write_ready_signal(file, &format!("ERROR:{e}\n"));
        }
    }

    result
}

/// Create the stdin transport and spawn a forwarding thread.
/// The transport is moved into the forwarding thread (it needs the server-side
/// handle on Windows). Cleanup is handled by `remove_stdin_transport`.
fn setup_stdin_forwarding(
    session_dir: &Path,
    child_stdin: Box<dyn Write + Send>,
    stdin_errors: &Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<()> {
    // StdinTransport is () on Unix — clippy flags the let-binding but
    // forward_stdin needs the value on Windows where the type is non-unit.
    #[allow(clippy::let_unit_value)]
    let transport = Current::create_stdin_transport(session_dir)?;

    // Spawn forwarding thread (detached -- not joined).
    // Thread owns the transport and exits when child stdin breaks or transport is removed.
    let session_dir_clone = session_dir.to_path_buf();
    let errors_clone = Arc::clone(stdin_errors);
    std::thread::spawn(move || {
        forward_stdin(transport, session_dir_clone, child_stdin, errors_clone)
    });

    Ok(())
}

/// Spawn a timeout thread that kills the child after `timeout_s` seconds.
/// Returns the `timed_out` flag. The caller passes a `cancel` flag to prevent the kill
/// after the child exits naturally.
///
/// Takes a `ChildKillHandle` (lightweight, Send + Clone) extracted from the
/// SupervisedChild, so the timeout thread uses the live backend context
/// (Job Object on Windows, process group on Unix) rather than degrading
/// to orphan-kill semantics.
fn setup_timeout(
    kill_handle: <Current as Platform>::ChildKillHandle,
    timeout_s: u64,
    cancel: Arc<AtomicBool>,
) -> Arc<AtomicBool> {
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_clone = Arc::clone(&timed_out);
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return; // Child exited before timeout -- don't kill
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if cancel.load(Ordering::Relaxed) {
            return; // Final check after deadline
        }
        timed_out_clone.store(true, Ordering::Relaxed);
        // Use kill_child with the live kill handle context.
        // On Windows this uses the Job Object for full tree kill;
        // on Unix this uses process group kill.
        let _ = Current::kill_child(&kill_handle, true);
    });
    timed_out
}

/// Spawn a thread that watches for a `kill_request` file from the CLI.
/// When found, validates the run_id matches (preventing stale requests from
/// killing a replacement run), then calls kill_child with the live
/// ChildKillHandle for tree-aware kill.
fn setup_kill_watcher(
    session_dir: &Path,
    kill_handle: <Current as Platform>::ChildKillHandle,
    run_id: RunId,
    cancel: Arc<AtomicBool>,
) {
    let kill_request_path = session_dir.join("kill_request");
    let kill_acted_path = session_dir.join("kill_acted");
    let run_id_str = run_id.to_string();
    std::thread::spawn(move || {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if kill_request_path.exists() {
                // Parse the request. If unreadable or malformed, discard it
                // rather than defaulting to force (avoids partial-read upgrades).
                let parsed = std::fs::read_to_string(&kill_request_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

                // Always clean up the request file.
                let _ = std::fs::remove_file(&kill_request_path);

                let request = match parsed {
                    Some(v) => v,
                    None => continue, // Malformed — ignore, CLI will retry or fall back
                };

                // Validate run_id — reject stale requests from a previous run.
                if request["run_id"].as_str() != Some(&run_id_str) {
                    continue; // Wrong run — ignore
                }

                let force = request["force"].as_bool().unwrap_or(false);

                // Leave a breadcrumb so exit classification knows this was
                // a sidecar-mediated kill (not a spontaneous child exit).
                // The kill_forced marker handles force=true classification;
                // this breadcrumb handles force=false → Killed.
                if !force {
                    let _ = std::fs::write(&kill_acted_path, "");
                }

                // Use the live kill handle for tree-aware kill.
                let _ = Current::kill_child(&kill_handle, force);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
}

/// Collect capture errors and stdin forwarding errors into a warning list.
fn collect_warnings(session_dir: &Path, stdin_errors: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    let mut warnings = Vec::new();

    // Collect capture errors
    let capture_err_path = session_dir.join("capture_errors.log");
    if let Ok(errors) = std::fs::read_to_string(&capture_err_path) {
        for line in errors.lines() {
            if !line.is_empty() {
                warnings.push(format!("log capture: {line}"));
            }
        }
    }

    // Collect stdin forwarding errors
    if let Ok(errs) = stdin_errors.lock() {
        for e in errs.iter() {
            warnings.push(e.clone());
        }
    }

    warnings
}

/// Outcome of the dependency wait phase.
enum DepWaitOutcome {
    /// All dependencies satisfied — proceed to spawn.
    Satisfied,
    /// A dependency failed (non-zero exit, not found, replaced).
    Failed(String),
    /// Timeout expired during the wait.
    TimedOut(String),
    /// Kill request received during the wait.
    Killed(String),
}

/// Poll dependency meta.json files until all reach terminal state.
fn wait_for_dependencies(
    session_root: &SessionRoot,
    namespace: &Namespace,
    spec: &LaunchSpec,
    timeout_s: Option<u64>,
    session_dir: &Path,
    run_id: &RunId,
) -> DepWaitOutcome {
    let deadline =
        timeout_s.map(|t| std::time::Instant::now() + std::time::Duration::from_secs(t));
    let kill_request_path = session_dir.join("kill_request");

    loop {
        // Check kill request first
        if kill_request_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&kill_request_path) {
                if let Ok(req) = serde_json::from_str::<serde_json::Value>(&content) {
                    if req["run_id"].as_str() == Some(&run_id.to_string()) {
                        let _ = std::fs::remove_file(&kill_request_path);
                        return DepWaitOutcome::Killed(
                            "killed during dependency wait".into(),
                        );
                    }
                }
            }
            // Wrong run_id or malformed — remove and ignore
            let _ = std::fs::remove_file(&kill_request_path);
        }

        // Check timeout
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                return DepWaitOutcome::TimedOut(
                    "timeout expired during dependency wait".into(),
                );
            }
        }

        // Poll all dependencies
        let mut all_satisfied = true;
        for dep in &spec.after {
            let dep_session = match session::open(session_root, namespace, &dep.session) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return DepWaitOutcome::Failed(format!(
                        "dependency session not found: {}",
                        dep.session
                    ));
                }
                Err(e) => {
                    return DepWaitOutcome::Failed(format!(
                        "failed to open dependency {}: {e}",
                        dep.session
                    ));
                }
            };

            let dep_meta = match session::read_meta(&dep_session) {
                Ok(m) => m,
                Err(e) => {
                    return DepWaitOutcome::Failed(format!(
                        "failed to read dependency {}: {e}",
                        dep.session
                    ));
                }
            };

            // Check run_id — reject if dependency was replaced
            if dep_meta.run_id() != dep.run_id {
                return DepWaitOutcome::Failed(format!(
                    "dependency {} was replaced (bound run_id {}, found {})",
                    dep.session,
                    dep.run_id,
                    dep_meta.run_id()
                ));
            }

            if dep_meta.status().is_terminal() {
                if !spec.after_any_exit {
                    use crate::model::state::{ExitReason as ER, RunStatus};
                    match dep_meta.status() {
                        RunStatus::Exited {
                            how: ER::ExitedOk, ..
                        } => {} // satisfied
                        _ => {
                            return DepWaitOutcome::Failed(format!(
                                "dependency {} exited with non-success state",
                                dep.session
                            ));
                        }
                    }
                }
                // satisfied (or --any-exit)
            } else {
                all_satisfied = false;
            }
        }

        if all_satisfied {
            return DepWaitOutcome::Satisfied;
        }

        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn run_inner(session_dir: &Path, ready: &mut Option<ReadyWriter>) -> anyhow::Result<()> {
    // --- Setup: lock, read spec, create meta ---
    let sidecar_identity = Current::self_identity()?;

    let session_name_str = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid session directory"))?;
    let session_name = SessionName::new(session_name_str)?;

    // Path structure: root/<namespace>/<session>/
    let ns_dir = session_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session dir has no parent (namespace)"))?;
    let namespace_str = ns_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid namespace directory"))?;
    let namespace = Namespace::new(namespace_str)?;

    let root = ns_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("namespace dir has no parent (root)"))?;
    let session_root = SessionRoot::new(root.to_path_buf());
    let session = session::open_raw(&session_root, &namespace, &session_name)?;

    let lock = LockGuard::try_acquire(&session)?;

    // Read launch spec
    let spec_path = session_dir.join("launch_spec.json");
    let spec_json =
        std::fs::read_to_string(&spec_path).context("failed to read launch_spec.json")?;
    let launch_spec: LaunchSpec =
        serde_json::from_str(&spec_json).context("invalid launch_spec.json")?;
    let _ = std::fs::remove_file(&spec_path);

    let run_id = RunId::new();
    let generation = {
        let gen_path = session_dir.join("generation");
        if let Ok(content) = std::fs::read_to_string(&gen_path) {
            let _ = std::fs::remove_file(&gen_path); // consumed
            content
                .trim()
                .parse::<u64>()
                .ok()
                .map(Generation::from_u64)
                .unwrap_or_else(Generation::first)
        } else {
            Generation::first()
        }
    };

    let mut meta = Meta::new_starting(
        session_name,
        run_id,
        generation,
        launch_spec,
        sidecar_identity,
        EpochTimestamp::now(),
    );

    // Re-set CLOEXEC on the ready fd before spawning the child.
    // The sidecar inherited this fd with CLOEXEC cleared (so it survived the sidecar's exec),
    // but the child must NOT hold the pipe open -- otherwise the CLI's read_to_string blocks
    // until the child exits, defeating the readiness handshake.
    if let Some(writer) = ready.take() {
        let sealed = Current::seal_ready_fd(writer)
            .map_err(|e| anyhow::anyhow!("failed to seal ready fd: {e}"))?;
        *ready = Some(sealed);
    }

    // Build effective env: user-supplied first, then TENDER_* overlay (authoritative).
    let mut effective_env = meta.launch_spec().env.clone();
    effective_env.insert(
        "TENDER_SESSION".to_owned(),
        meta.session().as_str().to_owned(),
    );
    effective_env.insert("TENDER_NAMESPACE".to_owned(), namespace.as_str().to_owned());
    effective_env.insert("TENDER_RUN_ID".to_owned(), run_id.to_string());
    effective_env.insert("TENDER_GENERATION".to_owned(), generation.to_string());
    effective_env.insert(
        "TENDER_SESSION_DIR".to_owned(),
        session_dir.to_str().unwrap_or("").to_owned(),
    );

    // --- Wait for --after dependencies ---
    let has_deps = !meta.launch_spec().after.is_empty();
    if has_deps {
        // Signal readiness BEFORE waiting — CLI unblocks, status shows Starting.
        session::write_meta_atomic(&session, &meta)?;
        signal_meta_snapshot(ready, &meta)?;

        match wait_for_dependencies(
            &session_root,
            &namespace,
            meta.launch_spec(),
            meta.launch_spec().timeout_s,
            session_dir,
            &run_id,
        ) {
            DepWaitOutcome::Satisfied => {} // proceed to spawn
            DepWaitOutcome::Failed(msg) => {
                meta.add_warning(msg);
                meta.transition_dependency_failed(EpochTimestamp::now(), DepFailReason::Failed)?;
                session::write_meta_atomic(&session, &meta)?;
                return Ok(());
            }
            DepWaitOutcome::TimedOut(msg) => {
                meta.add_warning(msg);
                meta.transition_dependency_failed(
                    EpochTimestamp::now(),
                    DepFailReason::TimedOut,
                )?;
                session::write_meta_atomic(&session, &meta)?;
                return Ok(());
            }
            DepWaitOutcome::Killed(msg) => {
                meta.add_warning(msg);
                meta.transition_dependency_failed(EpochTimestamp::now(), DepFailReason::Killed)?;
                session::write_meta_atomic(&session, &meta)?;
                return Ok(());
            }
        }
    }

    // --- Spawn child (with SpawnFailed handling inline) ---
    let stdin_piped = meta.launch_spec().stdin_mode == StdinMode::Pipe;
    let mut child = match Current::spawn_child(
        meta.launch_spec().argv(),
        stdin_piped,
        meta.launch_spec().cwd.as_deref(),
        &effective_env,
    ) {
        Ok(c) => c,
        Err(e) => {
            meta.add_warning(format!("spawn failed: {e}"));
            meta.transition_spawn_failed(EpochTimestamp::now())?;
            session::write_meta_atomic(&session, &meta)?;
            if !has_deps {
                signal_meta_snapshot(ready, &meta)?;
            }
            return Ok(()); // Not an error -- SpawnFailed is a valid terminal state
        }
    };

    // Get child identity -- need this before writing the orphan breadcrumb
    // so cleanup_orphan_dir can verify against PID reuse.
    let child_identity = match Current::child_identity(&child) {
        Ok(id) => id,
        Err(_) => {
            // Can't get identity -- kill and wait inline. No orphan is possible
            // since we kill synchronously, so don't write a breadcrumb.
            let handle = Current::child_kill_handle(&child);
            let _ = Current::kill_child(&handle, true);
            let _ = Current::child_wait(&mut child);
            meta.transition_spawn_failed(EpochTimestamp::now())?;
            session::write_meta_atomic(&session, &meta)?;
            if !has_deps {
                signal_meta_snapshot(ready, &meta)?;
            }
            return Ok(());
        }
    };

    // SAFETY: child_identity has been verified -- write it as the orphan breadcrumb.
    // If sidecar crashes after spawn but before meta write, the reconciler
    // can find and safely kill the orphaned child using this identity.
    let _ = std::fs::write(
        session_dir.join("child_pid"),
        serde_json::to_string(&child_identity).unwrap_or_default(),
    );

    // --- Stdin forwarding (conditional) ---
    let stdin_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    if meta.launch_spec().stdin_mode == StdinMode::Pipe {
        let child_stdin = Current::child_stdin(&mut child)
            .ok_or_else(|| anyhow::anyhow!("child stdin not piped"))?;
        setup_stdin_forwarding(session_dir, child_stdin, &stdin_errors)?;
    }

    // --- Transition to Running + readiness signal ---
    meta.transition_running(child_identity)?;
    session::write_meta_atomic(&session, &meta)?;
    if !has_deps {
        signal_meta_snapshot(ready, &meta)?;
    }

    // --- Timeout + kill watcher setup ---
    let kill_handle = Current::child_kill_handle(&child);
    let timeout_cancel = Arc::new(AtomicBool::new(false));
    let timed_out = if let Some(timeout_s) = meta.launch_spec().timeout_s {
        setup_timeout(kill_handle.clone(), timeout_s, Arc::clone(&timeout_cancel))
    } else {
        Arc::new(AtomicBool::new(false))
    };

    // Watch for CLI kill requests (kill_request file in session dir).
    // Uses the live ChildKillHandle for tree-aware kill on Windows.
    setup_kill_watcher(
        session_dir,
        kill_handle,
        run_id,
        Arc::clone(&timeout_cancel),
    );

    // --- Supervise ---
    let exit_reason = supervise(&session, &mut child)?;

    // --- Cancel timeout + collect warnings + determine exit reason ---
    timeout_cancel.store(true, Ordering::Relaxed);

    // Override reason if timeout fired (highest priority)
    let exit_reason = if timed_out.load(Ordering::Relaxed) {
        ExitReason::TimedOut
    } else {
        exit_reason
    };

    // Check for kill markers (lower priority than timeout).
    // Priority: TimedOut > KilledForced > Killed (from kill_acted) > raw exit.
    let kill_forced_path = session_dir.join("kill_forced");
    let kill_acted_path = session_dir.join("kill_acted");
    let exit_reason = if matches!(exit_reason, ExitReason::TimedOut) {
        // Timeout is highest priority — clean up markers but keep reason.
        let _ = std::fs::remove_file(&kill_forced_path);
        let _ = std::fs::remove_file(&kill_acted_path);
        exit_reason
    } else if kill_forced_path.exists() {
        let _ = std::fs::remove_file(&kill_forced_path);
        let _ = std::fs::remove_file(&kill_acted_path);
        ExitReason::KilledForced
    } else if kill_acted_path.exists() {
        // Sidecar-mediated graceful kill (force=false).
        // The child may report ExitedError on Windows (TerminateJobObject
        // after grace period), but the user requested a kill.
        let _ = std::fs::remove_file(&kill_acted_path);
        ExitReason::Killed
    } else {
        let _ = std::fs::remove_file(&kill_forced_path);
        let _ = std::fs::remove_file(&kill_acted_path);
        exit_reason
    };

    // Clean up kill_request if still present (kill watcher may not have run).
    let _ = std::fs::remove_file(session_dir.join("kill_request"));

    // Clean up stdin transport
    Current::remove_stdin_transport(session_dir);

    // Clean up breadcrumb -- no longer needed, meta has the child identity
    let _ = std::fs::remove_file(session_dir.join("child_pid"));

    for warning in collect_warnings(session_dir, &stdin_errors) {
        meta.add_warning(warning);
    }

    // --- Write terminal state (run state machine ends here) ---
    let exit_reason_debug = format!("{exit_reason:?}");
    meta.transition_exited(exit_reason, EpochTimestamp::now())?;
    session::write_meta_atomic(&session, &meta)?;

    // --- Release lock: session is now available for --replace ---
    drop(lock);

    // --- Execute on_exit callbacks (unlocked, separate from run lifecycle) ---
    let on_exit_callbacks = meta.launch_spec().on_exit.clone();
    if !on_exit_callbacks.is_empty() {
        let run_id = meta.run_id().to_string();
        let session_name = meta.session().as_str().to_string();
        let namespace = meta
            .launch_spec()
            .namespace
            .as_deref()
            .unwrap_or("default")
            .to_string();
        let generation = meta.generation().to_string();
        let session_dir_str = session_dir.to_str().unwrap_or("").to_string();

        let mut callback_results: Vec<serde_json::Value> = Vec::new();

        for (i, callback_cmd) in on_exit_callbacks.iter().enumerate() {
            let argv =
                shell_words::split(callback_cmd).unwrap_or_else(|_| vec![callback_cmd.clone()]);
            if argv.is_empty() {
                continue;
            }
            let result = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env("TENDER_SESSION", &session_name)
                .env("TENDER_NAMESPACE", &namespace)
                .env("TENDER_RUN_ID", &run_id)
                .env("TENDER_GENERATION", &generation)
                .env("TENDER_EXIT_REASON", &exit_reason_debug)
                .env("TENDER_SESSION_DIR", &session_dir_str)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();

            let record = match result {
                Ok(output) if output.status.success() => {
                    serde_json::json!({"index": i, "command": callback_cmd, "status": "ok"})
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    serde_json::json!({
                        "index": i,
                        "command": callback_cmd,
                        "status": "failed",
                        "exit_code": output.status.code(),
                        "stderr": stderr.trim()
                    })
                }
                Err(e) => {
                    serde_json::json!({
                        "index": i,
                        "command": callback_cmd,
                        "status": "spawn_failed",
                        "error": e.to_string()
                    })
                }
            };
            callback_results.push(record);
        }

        // Write callback results keyed by run_id, outside the session dir
        // This survives --replace (which removes the session dir)
        let callbacks_dir = session_dir
            .ancestors()
            .find(|p| p.ends_with("sessions"))
            .and_then(|p| p.parent())
            .map(|tender_root| tender_root.join("callbacks"));

        if let Some(dir) = callbacks_dir {
            let _ = std::fs::create_dir_all(&dir);
            let record = serde_json::json!({
                "run_id": run_id,
                "session": session_name,
                "namespace": namespace,
                "callbacks": callback_results
            });
            let _ = std::fs::write(dir.join(format!("{run_id}.json")), record.to_string());
        }
    }

    Ok(())
}

/// Forward data from the stdin transport to the child's stdin pipe.
/// Accepts connections in a loop to support multiple pushes.
/// Exits when: child stdin write fails (child exited) or transport is removed.
fn forward_stdin(
    transport: <Current as Platform>::StdinTransport,
    session_dir: PathBuf,
    mut child_stdin: Box<dyn Write + Send>,
    errors: Arc<Mutex<Vec<String>>>,
) {
    use std::io::Read;
    let mut buf = [0u8; 8192];
    loop {
        // Block until a writer connects (returns None if transport removed)
        let mut reader = match Current::accept_stdin_connection(&transport, &session_dir) {
            Some(r) => r,
            None => return, // transport closed/removed
        };
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break, // writer disconnected
                Ok(n) => n,
                Err(e) => {
                    if let Ok(mut errs) = errors.lock() {
                        errs.push(format!("stdin read failed: {e}"));
                    }
                    return;
                }
            };
            if child_stdin.write_all(&buf[..n]).is_err() {
                if let Ok(mut errs) = errors.lock() {
                    errs.push("stdin forwarding: child stdin closed".to_owned());
                }
                return;
            }
        }
    }
}

/// Send meta JSON over the readiness channel. Consumes the writer.
/// The CLI reads this snapshot directly -- no race with subsequent disk writes.
fn signal_meta_snapshot(ready: &mut Option<ReadyWriter>, meta: &Meta) -> anyhow::Result<()> {
    let writer = ready
        .take()
        .ok_or_else(|| anyhow::anyhow!("readiness channel already consumed"))?;
    let json = serde_json::to_string(meta)?;
    Current::write_ready_signal(writer, &format!("OK:{json}\n"))?;
    Ok(())
}

/// Supervise the child: capture stdout/stderr to output.log, wait for exit.
/// Returns the ExitReason when the child terminates.
fn supervise(
    session: &SessionDir,
    child: &mut <Current as Platform>::SupervisedChild,
) -> anyhow::Result<ExitReason> {
    let log_path = session.path().join("output.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log = Mutex::new(log_file);

    let stdout = Current::child_stdout(child).expect("stdout was piped");
    let stderr = Current::child_stderr(child).expect("stderr was piped");

    // Spawn reader threads. Capture errors rather than silently discarding.
    let log_ref = &log;
    let (stdout_result, stderr_result) = std::thread::scope(|scope| {
        let stdout_handle = scope.spawn(move || capture_stream(stdout, 'O', log_ref));
        let stderr_handle = scope.spawn(move || capture_stream(stderr, 'E', log_ref));

        let stdout_r = stdout_handle
            .join()
            .unwrap_or_else(|_| Err("stdout capture thread panicked".into()));
        let stderr_r = stderr_handle
            .join()
            .unwrap_or_else(|_| Err("stderr capture thread panicked".into()));
        (stdout_r, stderr_r)
    });

    // Log capture failures to a file in the session dir.
    // Sidecar stderr goes to /dev/null so eprintln is useless.
    // Don't fail supervision -- the child's exit status is still meaningful.
    let mut capture_errors = Vec::new();
    if let Err(e) = stdout_result {
        capture_errors.push(format!("stdout capture: {e}"));
    }
    if let Err(e) = stderr_result {
        capture_errors.push(format!("stderr capture: {e}"));
    }
    if !capture_errors.is_empty() {
        let err_path = session.path().join("capture_errors.log");
        let _ = std::fs::write(&err_path, capture_errors.join("\n"));
    }

    let status = Current::child_wait(child)?;

    let reason = match status.code() {
        Some(0) => ExitReason::ExitedOk,
        Some(code) => {
            let code = NonZeroI32::new(code).expect("already excluded zero");
            ExitReason::ExitedError { code }
        }
        None => ExitReason::Killed,
    };

    Ok(reason)
}

/// Read lines from a stream and write to the shared log file.
/// Returns an error if log writing fails persistently.
fn capture_stream(
    stream: Box<dyn std::io::Read + Send>,
    tag: char,
    log: &Mutex<File>,
) -> Result<(), String> {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // pipe closed
        };
        let ts = timestamp_micros();
        let formatted = format!("{ts} {tag} {line}\n");
        let mut f = log.lock().map_err(|e| format!("log mutex poisoned: {e}"))?;
        f.write_all(formatted.as_bytes())
            .map_err(|e| format!("log write failed: {e}"))?;
    }
    Ok(())
}

fn timestamp_micros() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let micros = duration.subsec_micros();
    format!("{secs}.{micros:06}")
}
