mod harness;

use harness::{tender, wait_running, wait_terminal};
use predicates::prelude::*;
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn wait_returns_terminal_state() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-ok", "true"])
        .assert()
        .success();
    wait_terminal(&root, "wait-ok");

    tender(&root)
        .args(["wait", "wait-ok"])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""status": "Exited"#))
        .stdout(predicate::str::contains(r#""reason": "ExitedOk"#));
}

#[test]
fn wait_blocks_until_exit() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-block", "sleep", "2"])
        .assert()
        .success();
    wait_running(&root, "wait-block");

    let start = std::time::Instant::now();
    tender(&root)
        .args(["wait", "wait-block"])
        .assert()
        .success();
    let elapsed = start.elapsed();

    assert!(elapsed > std::time::Duration::from_secs(1));
    assert!(elapsed < std::time::Duration::from_secs(5));
}

#[test]
fn wait_timeout_expires() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-timeout", "sleep", "60"])
        .assert()
        .success();
    wait_running(&root, "wait-timeout");

    let start = std::time::Instant::now();
    tender(&root)
        .args(["wait", "--timeout", "1", "wait-timeout"])
        .assert()
        .failure();
    assert!(start.elapsed().as_secs() < 3);

    tender(&root)
        .args(["kill", "--force", "wait-timeout"])
        .assert()
        .success();
}

#[test]
fn wait_nonexistent_session_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root).args(["wait", "nope"]).assert().failure();
}

#[test]
fn wait_exit_code_42_for_nonzero_child() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-err", "sh", "-c", "exit 3"])
        .assert()
        .success();
    wait_terminal(&root, "wait-err");

    tender(&root).args(["wait", "wait-err"]).assert().code(42);
}

#[test]
fn wait_exit_code_2_for_spawn_failed() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-spawn", "/nonexistent/binary"])
        .output()
        .unwrap(); // exit 2 from start is expected
    wait_terminal(&root, "wait-spawn");

    tender(&root).args(["wait", "wait-spawn"]).assert().code(2);
}
