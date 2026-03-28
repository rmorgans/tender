mod harness;

use harness::tender;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

/// Run `tender watch` with given args, let it poll for `timeout_secs`, then kill it
/// and return whatever it wrote to stdout.
fn run_watch_with_timeout(root: &TempDir, args: &[&str], timeout_secs: u64) -> String {
    let bin = assert_cmd::cargo::cargo_bin("tender");
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn tender watch");

    std::thread::sleep(std::time::Duration::from_secs(timeout_secs));
    let _ = child.kill();
    let output = child.wait_with_output().expect("failed to wait on child");
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Wait for meta.json to reach a terminal state under a specific namespace.
fn wait_terminal_ns(root: &TempDir, namespace: &str, session: &str) -> serde_json::Value {
    let path = root
        .path()
        .join(format!(".tender/sessions/{namespace}/{session}/meta.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                let status = meta["status"].as_str().unwrap_or("");
                if status != "Starting" && status != "Running" {
                    return meta;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for terminal state in {namespace}/{session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Wait for meta.json to reach terminal state in the default namespace.
fn wait_terminal_default(root: &TempDir, session: &str) -> serde_json::Value {
    wait_terminal_ns(root, "default", session)
}

#[test]
fn watch_emits_initial_state_snapshot() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a session and let it finish.
    tender(&root)
        .args(["start", "snap-echo", "--", "echo", "hi"])
        .output()
        .unwrap();
    wait_terminal_default(&root, "snap-echo");

    // Run watch — it should emit initial snapshot of the completed session.
    let output = run_watch_with_timeout(&root, &["watch", "--events"], 1);

    assert!(!output.is_empty(), "watch should emit at least one line");

    // Each line should be valid NDJSON.
    for line in output.lines() {
        let event: serde_json::Value =
            serde_json::from_str(line).expect("each line should be valid JSON");
        assert_eq!(event["source"], "tender.sidecar");
        assert_eq!(event["kind"], "run");
        assert!(
            event["ts"].is_f64() || event["ts"].is_u64(),
            "ts should be a number"
        );
        assert!(
            event["namespace"].is_string(),
            "namespace should be present"
        );
        assert!(event["session"].is_string(), "session should be present");
        assert!(event["run_id"].is_string(), "run_id should be present");
        assert!(event["name"].is_string(), "name should be present");
    }

    // The snapshot should be for snap-echo.
    let first: serde_json::Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();
    assert_eq!(first["session"], "snap-echo");
    assert_eq!(first["namespace"], "default");
    assert_eq!(first["kind"], "run");
    // Terminal session — should be run.exited.
    assert_eq!(first["name"], "run.exited");
}

#[test]
fn watch_emits_log_events() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a session that produces output.
    tender(&root)
        .args(["start", "log-echo", "--", "echo", "hello-watch"])
        .output()
        .unwrap();
    wait_terminal_default(&root, "log-echo");

    // Run watch with --logs only (no run events cluttering output).
    let output = run_watch_with_timeout(&root, &["watch", "--logs"], 1);

    assert!(!output.is_empty(), "watch should emit log events");

    // Find a log event containing the output.
    let mut found_log = false;
    for line in output.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if event["kind"] == "log" && event["name"] == "log.stdout" {
            let content = event["data"]["content"].as_str().unwrap_or("");
            if content.contains("hello-watch") {
                found_log = true;
            }
        }
    }
    assert!(
        found_log,
        "should find a log.stdout event with 'hello-watch', got:\n{output}"
    );
}

#[test]
fn watch_filters_by_namespace() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Create sessions in two namespaces.
    tender(&root)
        .args([
            "start",
            "alpha",
            "--namespace",
            "ns-a",
            "--",
            "echo",
            "a-out",
        ])
        .output()
        .unwrap();
    tender(&root)
        .args([
            "start",
            "beta",
            "--namespace",
            "ns-b",
            "--",
            "echo",
            "b-out",
        ])
        .output()
        .unwrap();
    wait_terminal_ns(&root, "ns-a", "alpha");
    wait_terminal_ns(&root, "ns-b", "beta");

    // Watch only ns-a.
    let output = run_watch_with_timeout(&root, &["watch", "--namespace", "ns-a", "--events"], 1);

    assert!(!output.is_empty(), "should emit events for ns-a");

    for line in output.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            event["namespace"], "ns-a",
            "all events should be from ns-a, got: {event}"
        );
    }
}

#[test]
fn watch_from_now_skips_existing() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start and complete a session before running watch.
    tender(&root)
        .args(["start", "old-session", "--", "echo", "old"])
        .output()
        .unwrap();
    wait_terminal_default(&root, "old-session");

    // Run watch with --from-now — should not emit any events for the already-terminal session.
    let output = run_watch_with_timeout(&root, &["watch", "--from-now"], 1);

    // There should be no run events for old-session.
    for line in output.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if event["kind"] == "run" {
            panic!(
                "from-now should not emit run events for existing terminal sessions, got: {event}"
            );
        }
        // Log events from existing content should also be skipped.
        if event["kind"] == "log" {
            let content = event["data"]["content"].as_str().unwrap_or("");
            assert!(
                !content.contains("old"),
                "from-now should skip existing log lines, got: {event}"
            );
        }
    }
}

#[test]
fn watch_both_events_and_logs_by_default() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a session that produces output.
    tender(&root)
        .args(["start", "both-echo", "--", "echo", "dual-output"])
        .output()
        .unwrap();
    wait_terminal_default(&root, "both-echo");

    // Run watch with no --events or --logs flags — should emit both.
    let output = run_watch_with_timeout(&root, &["watch"], 1);

    let mut has_run = false;
    let mut has_log = false;
    for line in output.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        match event["kind"].as_str() {
            Some("run") => has_run = true,
            Some("log") => has_log = true,
            _ => {}
        }
    }
    assert!(
        has_run,
        "default watch should emit run events, got:\n{output}"
    );
    assert!(
        has_log,
        "default watch should emit log events, got:\n{output}"
    );
}

#[test]
fn watch_from_now_includes_sessions_started_after_watch() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start watch --from-now FIRST (before any sessions exist)
    let bin = assert_cmd::cargo::cargo_bin("tender");
    let mut watch_child = Command::new(bin)
        .args(["watch", "--from-now"])
        .env("HOME", root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn watch");

    // Give watch time to start polling
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Now start a session AFTER watch is running
    tender(&root)
        .args(["start", "after-watch", "--", "echo", "post-watch-output"])
        .output()
        .unwrap();
    wait_terminal_default(&root, "after-watch");

    // Give watch time to discover and emit
    std::thread::sleep(std::time::Duration::from_secs(1));
    let _ = watch_child.kill();
    let output = watch_child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Sessions started AFTER watch should be visible (not skipped by --from-now)
    let has_run_event = stdout.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .map(|e| e["kind"] == "run" && e["session"] == "after-watch")
            .unwrap_or(false)
    });
    assert!(
        has_run_event,
        "watch --from-now should include sessions started after watch began, got:\n{stdout}"
    );
}

#[test]
fn watch_detects_replace_and_resets_log_offset() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a session that produces output
    tender(&root)
        .args(["start", "replace-watch", "--", "echo", "first-run-output"])
        .output()
        .unwrap();
    wait_terminal_default(&root, "replace-watch");

    // Start watch (includes initial snapshot)
    let bin = assert_cmd::cargo::cargo_bin("tender");
    let mut watch_child = Command::new(&bin)
        .args(["watch"])
        .env("HOME", root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn watch");

    // Give watch time to read initial state + logs
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Replace with a new run that produces different output
    tender(&root)
        .args([
            "start",
            "replace-watch",
            "--replace",
            "--",
            "echo",
            "second-run-output",
        ])
        .output()
        .unwrap();
    wait_terminal_default(&root, "replace-watch");

    // Give watch time to detect replacement
    std::thread::sleep(std::time::Duration::from_secs(1));
    let _ = watch_child.kill();
    let output = watch_child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should see second-run-output in log events (offset was reset on run_id change)
    let has_second_run_log = stdout.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .map(|e| {
                e["kind"] == "log"
                    && e["data"]["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("second-run-output"))
            })
            .unwrap_or(false)
    });
    assert!(
        has_second_run_log,
        "watch should see logs from replaced run (offset reset), got:\n{stdout}"
    );
}
