mod harness;

use harness::{tender, wait_running, wait_terminal};
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn timeout_kills_child() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let start = std::time::Instant::now();
    tender(&root)
        .args(["start", "--timeout", "2", "timeout-job", "sleep", "60"])
        .assert()
        .success();
    wait_running(&root, "timeout-job");

    let meta = wait_terminal(&root, "timeout-job");
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "TimedOut");
}

#[test]
fn timeout_not_triggered() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "--timeout", "60", "fast-job", "true"])
        .assert()
        .success();

    let meta = wait_terminal(&root, "fast-job");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedOk");
}
