mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// --after resolves dependency run_id at bind time.
/// Verifies the launch_spec in meta.json contains the binding.
#[test]
fn after_bind_captures_run_id() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (short-lived)
    harness::tender(&root)
        .args(["start", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    // Read job1's run_id
    let job1_meta_path = root
        .path()
        .join(".tender/sessions/default/job1/meta.json");
    let job1_meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&job1_meta_path).unwrap()).unwrap();
    let job1_run_id = job1_meta["run_id"].as_str().unwrap();

    // Start job2 --after job1
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job2");

    // Verify job2's launch_spec.after contains job1's run_id
    let job2_meta_path = root
        .path()
        .join(".tender/sessions/default/job2/meta.json");
    let job2_meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&job2_meta_path).unwrap()).unwrap();
    let after = &job2_meta["launch_spec"]["after"];
    assert_eq!(after[0]["session"].as_str(), Some("job1"));
    assert_eq!(after[0]["run_id"].as_str(), Some(job1_run_id));
}

/// --after nonexistent session fails at bind time.
#[test]
fn after_nonexistent_session_fails_at_bind() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job2", "--after", "nonexistent", "--", "true"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session not found"));
}

/// Idempotent start on a Starting session with --after: same spec -> return existing.
/// With the sidecar wait loop, --after sessions stay in Starting while waiting.
/// This validates that the after bindings are part of the spec hash for idempotent matching.
#[test]
fn after_idempotent_on_running() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (long-running)
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1 (stays in Starting, waiting for job1)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "sleep", "30"])
        .assert()
        .success();

    // Give sidecar time to enter wait loop
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Second start with identical args: should succeed (idempotent)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "sleep", "30"])
        .assert()
        .success();

    // Clean up
    let _ = harness::tender(&root)
        .args(["kill", "job1", "--force"])
        .assert();
    let _ = harness::tender(&root)
        .args(["kill", "job2"])
        .assert();
}

/// Idempotent start on Starting session (waiting for deps): same spec -> return existing.
#[test]
fn after_idempotent_on_starting() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (long-running so job2 stays in Starting)
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1 (enters Starting, waits)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "echo", "done"])
        .assert()
        .success();

    // Give sidecar time to enter wait loop
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Second start with identical args: should succeed (idempotent)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "echo", "done"])
        .assert()
        .success();

    // Clean up
    let _ = harness::tender(&root)
        .args(["kill", "job1", "--force"])
        .assert();
    let _ = harness::tender(&root)
        .args(["kill", "job2"])
        .assert();
}

/// job2 waits while job1 is still running, then runs after job1 exits.
#[test]
fn after_waits_for_running_dependency() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (runs for 2s)
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "2"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();

    // job2 should be Starting (waiting)
    let meta_path = root
        .path()
        .join(".tender/sessions/default/job2/meta.json");
    let content = std::fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(meta["status"].as_str(), Some("Starting"));

    // Wait for both to finish
    harness::wait_terminal(&root, "job1");
    let meta2 = harness::wait_terminal(&root, "job2");
    assert_eq!(meta2["status"].as_str(), Some("Exited"));
    assert_eq!(meta2["reason"].as_str(), Some("ExitedOk"));
}
