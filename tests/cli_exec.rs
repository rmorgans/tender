mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// exec fails if session doesn't exist.
#[test]
fn exec_session_not_found() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["exec", "nonexistent", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session not found"));
}

/// exec fails if session is not running (terminal state).
#[test]
fn exec_session_not_running() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");
    harness::tender(&root)
        .args(["exec", "job1", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not running"));
}

/// exec fails if session lacks --stdin.
#[test]
fn exec_session_no_stdin() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");
    harness::tender(&root)
        .args(["exec", "job1", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--stdin"));
    let _ = harness::tender(&root)
        .args(["kill", "job1", "--force"])
        .assert();
}
