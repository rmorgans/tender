use std::collections::HashMap;
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
    kind: &str,
    name: &str,
    data: serde_json::Value,
) -> bool {
    let event = serde_json::json!({
        "ts": ts,
        "namespace": namespace,
        "session": session,
        "run_id": run_id,
        "source": "tender.sidecar",
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
    from_now: bool,
) -> anyhow::Result<()> {
    let root = SessionRoot::default_path()?;

    // If neither --events nor --logs specified, emit both.
    let emit_events = events || !logs;
    let emit_logs = logs || !events;

    let mut watchers: HashMap<(String, String), SessionWatcher> = HashMap::new();
    let mut stdout = io::BufWriter::new(io::stdout().lock());

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
                // Existing session — check for status change.
                if emit_events && watcher.last_status != current_status_key {
                    // Also handle run_id change (session restarted).
                    watcher.run_id = run_id_str.clone();
                    watcher.last_status = current_status_key;
                    let ts = now_epoch_secs();
                    if !emit_event(
                        &mut stdout,
                        ts,
                        &watcher.namespace,
                        &watcher.session,
                        &watcher.run_id,
                        "run",
                        run_event_name(meta.status()),
                        run_event_data(meta.status()),
                    ) {
                        return Ok(());
                    }
                }
            } else {
                // New session discovered.
                let watcher = SessionWatcher {
                    namespace: ns.as_str().to_owned(),
                    session: name.as_str().to_owned(),
                    run_id: run_id_str,
                    last_status: current_status_key,
                    last_log_offset: 0,
                };

                if !from_now && emit_events {
                    // Emit initial snapshot of current state.
                    let ts = now_epoch_secs();
                    if !emit_event(
                        &mut stdout,
                        ts,
                        &watcher.namespace,
                        &watcher.session,
                        &watcher.run_id,
                        "run",
                        run_event_name(meta.status()),
                        run_event_data(meta.status()),
                    ) {
                        return Ok(());
                    }
                }

                watchers.insert(key.clone(), watcher);

                if from_now {
                    // Skip existing log content — seek to end.
                    let log_path = session_dir.path().join("output.log");
                    if let Ok(file_meta) = std::fs::metadata(&log_path) {
                        watchers.get_mut(&key).unwrap().last_log_offset = file_meta.len();
                    }
                }
            }

            // Read new log lines.
            if emit_logs {
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
                                        if let Some(parsed) = LogLine::parse(trimmed) {
                                            let ts_secs = parsed.timestamp_us as f64 / 1_000_000.0;
                                            let log_name = if parsed.tag == 'O' {
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
                                                "log",
                                                log_name,
                                                serde_json::json!({"content": parsed.content}),
                                            ) {
                                                return Ok(());
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

        std::thread::sleep(Duration::from_millis(100));
    }
}
