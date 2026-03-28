mod harness;

use harness::{tender, wait_running, wait_terminal};
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

/// Wait for meta.json to reach terminal, then wait a bit more for callback
/// re-write (callbacks execute after the first terminal write).
fn wait_terminal_with_callbacks(root: &TempDir, session: &str) -> serde_json::Value {
    // First wait for terminal state
    let _meta = wait_terminal(root, session);

    // Callbacks run after terminal state is written, then meta is re-written.
    // Give callbacks time to execute and the re-write to land.
    let path = root
        .path()
        .join(format!(".tender/sessions/default/{session}/meta.json"));
    // Small grace period for callback execution + atomic re-write
    std::thread::sleep(std::time::Duration::from_millis(500));

    let content = std::fs::read_to_string(&path).expect("meta.json should exist");
    serde_json::from_str(&content).expect("meta.json should be valid JSON")
}

#[test]
fn on_exit_callback_runs_after_normal_exit() {
    let _guard = SERIAL.lock().unwrap();
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

    wait_terminal_with_callbacks(&root, "on-exit-normal");

    assert!(
        marker.exists(),
        "marker file should exist after normal exit callback: {marker:?}"
    );
}

#[test]
fn on_exit_callback_runs_after_forced_kill() {
    let _guard = SERIAL.lock().unwrap();
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

    let kill_out = tender(&root)
        .args(["kill", "--force", "on-exit-kill"])
        .output()
        .unwrap();
    assert!(
        kill_out.status.success(),
        "kill failed: {}",
        String::from_utf8_lossy(&kill_out.stderr)
    );

    wait_terminal_with_callbacks(&root, "on-exit-kill");

    assert!(
        marker.exists(),
        "marker file should exist after forced kill callback: {marker:?}"
    );
}

#[test]
fn on_exit_callback_sees_env_vars() {
    let _guard = SERIAL.lock().unwrap();
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

    wait_terminal_with_callbacks(&root, "on-exit-env");

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
fn on_exit_callback_failure_adds_warning_not_state_change() {
    let _guard = SERIAL.lock().unwrap();
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

    let meta = wait_terminal_with_callbacks(&root, "on-exit-fail");

    // Status should still be the correct terminal state (Exited with ExitedOk)
    assert_eq!(
        meta["status"].as_str(),
        Some("Exited"),
        "status should be Exited, got: {}",
        serde_json::to_string_pretty(&meta).unwrap()
    );
    assert_eq!(
        meta["reason"].as_str(),
        Some("ExitedOk"),
        "reason should be ExitedOk, got: {}",
        serde_json::to_string_pretty(&meta).unwrap()
    );

    // Warnings should contain an entry about the failed callback
    let warnings = meta["warnings"]
        .as_array()
        .expect("warnings should be an array");
    assert!(
        !warnings.is_empty(),
        "warnings should not be empty after failed callback"
    );
    let has_on_exit_warning = warnings
        .iter()
        .any(|w| w.as_str().is_some_and(|s| s.contains("on_exit[0]")));
    assert!(
        has_on_exit_warning,
        "warnings should contain on_exit[0] failure entry, got: {warnings:?}"
    );
}

#[test]
fn on_exit_multiple_callbacks_run_in_order() {
    let _guard = SERIAL.lock().unwrap();
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

    wait_terminal_with_callbacks(&root, "on-exit-multi");

    assert!(
        marker_a.exists(),
        "first marker file should exist: {marker_a:?}"
    );
    assert!(
        marker_b.exists(),
        "second marker file should exist: {marker_b:?}"
    );
}
