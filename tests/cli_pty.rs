#![cfg(unix)]

mod harness;

use std::sync::Mutex;
use harness::tender;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn start_pty_flag_sets_io_mode() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let output = tender(&root)
        .args(["start", "pty-test", "--pty", "--", "echo", "hello"])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let meta: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(meta["launch_spec"]["io_mode"], "Pty");
}

#[test]
fn exec_rejected_on_pty_session() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "pty-shell", "--pty", "--stdin", "--", "sleep", "60"])
        .output()
        .unwrap();
    harness::wait_running(&root, "pty-shell");

    let output = tender(&root)
        .args(["exec", "pty-shell", "--", "echo", "test"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not supported") || stderr.contains("PTY"),
        "should reject exec on PTY: {stderr}");

    tender(&root).args(["kill", "pty-shell"]).output().ok();
}
