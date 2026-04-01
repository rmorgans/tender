mod harness;

use harness::{tender, wait_running, wait_terminal};
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

/// Wait for terminal state, then wait for callback completion.
/// Callbacks run after the lock is released, so we poll for the callback record file.
fn wait_for_callbacks(root: &TempDir, session: &str) {
    wait_terminal(root, session);

    // Callbacks run after lock release. Poll for the callback record file.
    // Read meta to get run_id, then check for callbacks/<run_id>.json
    let meta_path = root
        .path()
        .join(format!(".tender/sessions/default/{session}/meta.json"));
    let content = std::fs::read_to_string(&meta_path).expect("meta.json should exist");
    let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
    let run_id = meta["run_id"].as_str().unwrap();

    let callbacks_path = root.path().join(format!(".tender/callbacks/{run_id}.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if callbacks_path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Callbacks may have no failures and still write the record, so not finding
    // the file just means we timed out waiting — tests that need it will assert.
}

/// Read the callback record for a session (by run_id from meta.json).
fn read_callback_record(root: &TempDir, session: &str) -> Option<serde_json::Value> {
    let meta_path = root
        .path()
        .join(format!(".tender/sessions/default/{session}/meta.json"));
    let content = std::fs::read_to_string(&meta_path).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&content).ok()?;
    let run_id = meta["run_id"].as_str()?;

    let callbacks_path = root.path().join(format!(".tender/callbacks/{run_id}.json"));
    let cb_content = std::fs::read_to_string(&callbacks_path).ok()?;
    serde_json::from_str(&cb_content).ok()
}

#[test]
fn on_exit_callback_runs_after_normal_exit() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let marker = root.path().join("normal_exit_marker");

    let out = tender(&root)
        .args([
            "start",
            "on-exit-normal",
            "--on-exit",
            &format!("touch {}", marker.display()),
            "--",
            "echo",
            "done",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_for_callbacks(&root, "on-exit-normal");

    assert!(
        marker.exists(),
        "marker file should exist after normal exit callback: {marker:?}"
    );
}

#[test]
fn on_exit_callback_runs_after_forced_kill() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let marker = root.path().join("kill_exit_marker");

    let out = tender(&root)
        .args([
            "start",
            "on-exit-kill",
            "--on-exit",
            &format!("touch {}", marker.display()),
            "--",
            "sleep",
            "60",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_running(&root, "on-exit-kill");

    tender(&root)
        .args(["kill", "--force", "on-exit-kill"])
        .assert()
        .success();

    wait_for_callbacks(&root, "on-exit-kill");

    assert!(
        marker.exists(),
        "marker file should exist after forced kill callback: {marker:?}"
    );
}

#[test]
fn on_exit_callback_sees_env_vars() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let output_file = root.path().join("env_output.txt");

    let on_exit_cmd = format!(
        "sh -c \"echo $TENDER_SESSION $TENDER_NAMESPACE $TENDER_EXIT_REASON > {}\"",
        output_file.display()
    );

    let out = tender(&root)
        .args([
            "start",
            "on-exit-env",
            "--on-exit",
            &on_exit_cmd,
            "--",
            "echo",
            "done",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_for_callbacks(&root, "on-exit-env");

    assert!(
        output_file.exists(),
        "env output file should exist: {output_file:?}"
    );

    let content = std::fs::read_to_string(&output_file).unwrap();
    let content = content.trim();

    assert!(
        content.contains("on-exit-env"),
        "should contain session name 'on-exit-env', got: {content}"
    );
    assert!(
        content.contains("default"),
        "should contain namespace 'default', got: {content}"
    );
    assert!(
        content.contains("ExitedOk"),
        "should contain exit reason 'ExitedOk', got: {content}"
    );
}

#[test]
fn on_exit_callback_failure_recorded_without_state_change() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .args([
            "start",
            "on-exit-fail",
            "--on-exit",
            "/nonexistent/binary",
            "--",
            "echo",
            "done",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_for_callbacks(&root, "on-exit-fail");

    // Meta should still show correct terminal state — callbacks don't modify it
    let meta_path = root
        .path()
        .join(".tender/sessions/default/on-exit-fail/meta.json");
    let meta_content = std::fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta_content).unwrap();
    assert_eq!(meta["status"].as_str(), Some("Exited"));

    // Callback failure should be in the callback record, not meta.json warnings
    let record = read_callback_record(&root, "on-exit-fail").expect("callback record should exist");
    let callbacks = record["callbacks"].as_array().expect("callbacks array");
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0]["status"].as_str(), Some("spawn_failed"));
}

#[test]
fn on_exit_multiple_callbacks_both_run() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let marker_a = root.path().join("marker_a");
    let marker_b = root.path().join("marker_b");

    let out = tender(&root)
        .args([
            "start",
            "on-exit-multi",
            "--on-exit",
            &format!("touch {}", marker_a.display()),
            "--on-exit",
            &format!("touch {}", marker_b.display()),
            "--",
            "echo",
            "done",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_for_callbacks(&root, "on-exit-multi");

    assert!(
        marker_a.exists(),
        "first marker file should exist: {marker_a:?}"
    );
    assert!(
        marker_b.exists(),
        "second marker file should exist: {marker_b:?}"
    );
}
