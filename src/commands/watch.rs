use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tender::log::LogLine;
use tender::model::ids::Namespace;
use tender::model::state::{ExitReason, RunStatus};
use tender::session::{self, SessionRoot};

/// Per-session polling state.
struct SessionWatcher {
    namespace: String,
    session: String,
    run_id: String,
    last_status: String,
    last_log_offset: u64,
}

/// Return epoch seconds with microsecond precision as an f64.
fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Map a RunStatus to its event name string.
fn run_event_name(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Starting => "run.starting",
        RunStatus::Running { .. } => "run.started",
        RunStatus::SpawnFailed { .. } => "run.spawn_failed",
        RunStatus::Exited { how, .. } => match how {
            ExitReason::ExitedOk => "run.exited",
            ExitReason::ExitedError { .. } => "run.exited",
            ExitReason::Killed => "run.killed",
            ExitReason::KilledForced => "run.killed",
            ExitReason::TimedOut => "run.timed_out",
        },
        RunStatus::SidecarLost { .. } => "run.sidecar_lost",
        RunStatus::DependencyFailed { .. } => "run.dependency_failed",
    }
}

/// Build the data object for a run event.
fn run_event_data(status: &RunStatus) -> serde_json::Value {
    match status {
        RunStatus::Starting => serde_json::json!({"status": "Starting"}),
        RunStatus::Running { .. } => serde_json::json!({"status": "Running"}),
        RunStatus::SpawnFailed { .. } => serde_json::json!({"status": "SpawnFailed"}),
        RunStatus::Exited { how, .. } => match how {
            ExitReason::ExitedOk => {
                serde_json::json!({"status": "Exited", "reason": "ExitedOk", "exit_code": 0})
            }
            ExitReason::ExitedError { code } => {
                serde_json::json!({"status": "Exited", "reason": "ExitedError", "exit_code": code.get()})
            }
            ExitReason::Killed => {
                serde_json::json!({"status": "Exited", "reason": "Killed"})
            }
            ExitReason::KilledForced => {
                serde_json::json!({"status": "Exited", "reason": "KilledForced"})
            }
            ExitReason::TimedOut => {
                serde_json::json!({"status": "Exited", "reason": "TimedOut"})
            }
        },
        RunStatus::SidecarLost { .. } => {
            serde_json::json!({"status": "SidecarLost"})
        }
        RunStatus::DependencyFailed { reason, .. } => {
            use tender::model::dep_fail::DepFailReason;
            let reason_str = match reason {
                DepFailReason::Failed => "Failed",
                DepFailReason::TimedOut => "TimedOut",
                DepFailReason::Killed => "Killed",
                DepFailReason::KilledForced => "KilledForced",
            };
            serde_json::json!({"status": "DependencyFailed", "reason": reason_str})
        }
    }
}

/// Serialize a status to a string for dedup comparison.
fn status_key(status: &RunStatus) -> String {
    // Serialize the status to JSON for a stable comparison key.
    serde_json::to_string(status).unwrap_or_default()
}

/// Emit one NDJSON event line to stdout. Returns false if stdout is broken (pipe closed).
#[allow(clippy::too_many_arguments)]
fn emit_event(
    out: &mut impl Write,
    ts: f64,
    namespace: &str,
    session: &str,
    run_id: &str,
    source: &str,
    kind: &str,
    name: &str,
    data: serde_json::Value,
) -> bool {
    let event = serde_json::json!({
        "ts": ts,
        "namespace": namespace,
        "session": session,
        "run_id": run_id,
        "source": source,
        "kind": kind,
        "name": name,
        "data": data,
    });
    let line = serde_json::to_string(&event).expect("JSON serialization cannot fail");
    if writeln!(out, "{line}").is_err() {
        return false;
    }
    if out.flush().is_err() {
        return false;
    }
    true
}

pub fn cmd_watch(
    namespace: Option<&Namespace>,
    events: bool,
    logs: bool,
    annotations: bool,
    from_now: bool,
) -> anyhow::Result<()> {
    let root = SessionRoot::default_path()?;

    // If no filter flags specified, emit events + logs (not annotations).
    // Annotations require explicit --annotations.
    let any_filter = events || logs || annotations;
    let emit_events = events || !any_filter;
    let emit_logs = logs || !any_filter;
    let emit_annotations = annotations;

    let mut watchers: HashMap<(String, String), SessionWatcher> = HashMap::new();
    let mut stdout = io::BufWriter::new(io::stdout().lock());

    // Record which sessions exist at watch invocation time.
    // --from-now skips initial state only for these, not for sessions discovered later.
    let initial_sessions: HashSet<(String, String)> = if from_now {
        session::list(&root, namespace)
            .unwrap_or_default()
            .iter()
            .map(|(ns, name)| (ns.as_str().to_owned(), name.as_str().to_owned()))
            .collect()
    } else {
        HashSet::new()
    };

    loop {
        // Discover sessions.
        let sessions = session::list(&root, namespace).unwrap_or_default();

        for (ns, name) in &sessions {
            let key = (ns.as_str().to_owned(), name.as_str().to_owned());

            // Try to open the session and read meta.
            let session_dir = match session::open(&root, ns, name) {
                Ok(Some(dir)) => dir,
                _ => continue,
            };
            let meta = match session::read_meta(&session_dir) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let run_id_str = meta.run_id().to_string();
            let current_status_key = status_key(meta.status());

            if let Some(watcher) = watchers.get_mut(&key) {
                // Detect run_id change (session replaced) — reset log offset
                if watcher.run_id != run_id_str {
                    watcher.run_id = run_id_str.clone();
                    watcher.last_log_offset = 0;
                    watcher.last_status = String::new(); // force status re-emit
                }

                // Check for status change.
                if emit_events && watcher.last_status != current_status_key {
                    watcher.last_status = current_status_key;
                    let ts = now_epoch_secs();
                    if !emit_event(
                        &mut stdout,
                        ts,
                        &watcher.namespace,
                        &watcher.session,
                        &watcher.run_id,
                        "tender.sidecar",
                        "run",
                        run_event_name(meta.status()),
                        run_event_data(meta.status()),
                    ) {
                        return Ok(());
                    }
                }
            } else {
                // New session discovered.
                let skip_initial = initial_sessions.contains(&key);

                let watcher = SessionWatcher {
                    namespace: ns.as_str().to_owned(),
                    session: name.as_str().to_owned(),
                    run_id: run_id_str,
                    last_status: current_status_key,
                    last_log_offset: 0,
                };

                if !skip_initial && emit_events {
                    // Emit initial snapshot of current state.
                    let ts = now_epoch_secs();
                    if !emit_event(
                        &mut stdout,
                        ts,
                        &watcher.namespace,
                        &watcher.session,
                        &watcher.run_id,
                        "tender.sidecar",
                        "run",
                        run_event_name(meta.status()),
                        run_event_data(meta.status()),
                    ) {
                        return Ok(());
                    }
                }

                watchers.insert(key.clone(), watcher);

                if skip_initial {
                    // Skip existing log content — seek to end.
                    let log_path = session_dir.path().join("output.log");
                    if let Ok(file_meta) = std::fs::metadata(&log_path) {
                        watchers.get_mut(&key).unwrap().last_log_offset = file_meta.len();
                    }
                }
            }

            // Read new log lines.
            if emit_logs || emit_annotations {
                let watcher = watchers.get_mut(&key).unwrap();
                let log_path = session_dir.path().join("output.log");
                if log_path.exists() {
                    if let Ok(file) = std::fs::File::open(&log_path) {
                        let mut reader = BufReader::new(file);
                        if reader
                            .seek(SeekFrom::Start(watcher.last_log_offset))
                            .is_ok()
                        {
                            let mut buf = String::new();
                            loop {
                                buf.clear();
                                match reader.read_line(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        watcher.last_log_offset += n as u64;
                                        let trimmed =
                                            buf.trim_end_matches('\n').trim_end_matches('\r');
                                        if let Ok(parsed) = serde_json::from_str::<LogLine>(trimmed)
                                        {
                                            let ts_secs = parsed.ts;
                                            match parsed.tag.as_str() {
                                                "O" | "E" if emit_logs => {
                                                    let log_name = if parsed.tag == "O" {
                                                        "log.stdout"
                                                    } else {
                                                        "log.stderr"
                                                    };
                                                    if !emit_event(
                                                        &mut stdout,
                                                        ts_secs,
                                                        &watcher.namespace,
                                                        &watcher.session,
                                                        &watcher.run_id,
                                                        "tender.sidecar",
                                                        "log",
                                                        log_name,
                                                        serde_json::json!({"content": parsed.format_raw()}),
                                                    ) {
                                                        return Ok(());
                                                    }
                                                }
                                                "A" if emit_annotations => {
                                                    if !parsed.content.is_null() {
                                                        let ann = parsed.content.clone();
                                                        let source = ann["source"]
                                                            .as_str()
                                                            .unwrap_or("unknown")
                                                            .to_owned();
                                                        let event_name = ann["event"]
                                                            .as_str()
                                                            .unwrap_or("unknown")
                                                            .to_owned();
                                                        let name =
                                                            format!("annotation.{event_name}");
                                                        if !emit_event(
                                                            &mut stdout,
                                                            ts_secs,
                                                            &watcher.namespace,
                                                            &watcher.session,
                                                            &watcher.run_id,
                                                            &source,
                                                            "annotation",
                                                            &name,
                                                            ann,
                                                        ) {
                                                            return Ok(());
                                                        }
                                                    }
                                                }
                                                _ => {} // skip if not emitting this type
                                            }
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                }
            }
        }

        // Prune watchers for sessions that no longer exist on disk
        let current_keys: HashSet<(String, String)> = sessions
            .iter()
            .map(|(ns, name)| (ns.as_str().to_owned(), name.as_str().to_owned()))
            .collect();
        watchers.retain(|key, _| current_keys.contains(key));

        std::thread::sleep(Duration::from_millis(100));
    }
}
