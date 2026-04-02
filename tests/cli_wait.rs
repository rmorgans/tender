mod harness;

use harness::{tender, wait_running, wait_terminal};
use predicates::prelude::*;
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn wait_returns_terminal_state() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-ok", "true"])
        .assert()
        .success();
    wait_terminal(&root, "wait-ok");

    // Output is now a JSON array (even for single session).
    tender(&root)
        .args(["wait", "wait-ok"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("["))
        .stdout(predicate::str::contains(r#""status": "Exited"#))
        .stdout(predicate::str::contains(r#""reason": "ExitedOk"#));
}

#[test]
fn wait_blocks_until_exit() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
        .failure()
        .code(1)
        .stderr(predicate::str::contains("timeout"));
    assert!(start.elapsed().as_secs() < 3);

    tender(&root)
        .args(["kill", "--force", "wait-timeout"])
        .assert()
        .success();
}

#[test]
fn wait_nonexistent_session_fails() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root).args(["wait", "nope"]).assert().failure();
}

#[test]
fn wait_exit_code_42_for_nonzero_child() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "wait-spawn", "/nonexistent/binary"])
        .output()
        .unwrap(); // exit 2 from start is expected
    wait_terminal(&root, "wait-spawn");

    tender(&root).args(["wait", "wait-spawn"]).assert().code(2);
}

// --- New multi-session tests ---

#[test]
fn wait_all_multiple_sessions() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "multi-a", "true"])
        .assert()
        .success();
    tender(&root)
        .args(["start", "multi-b", "true"])
        .assert()
        .success();
    wait_terminal(&root, "multi-a");
    wait_terminal(&root, "multi-b");

    let output = tender(&root)
        .args(["wait", "multi-a", "multi-b"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["session"], "multi-a");
    assert_eq!(arr[1]["session"], "multi-b");
}

#[test]
fn wait_any_returns_on_first_terminal() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // One fast, one slow.
    tender(&root)
        .args(["start", "any-fast", "true"])
        .assert()
        .success();
    tender(&root)
        .args(["start", "any-slow", "sleep", "60"])
        .assert()
        .success();
    wait_terminal(&root, "any-fast");
    wait_running(&root, "any-slow");

    let start = std::time::Instant::now();
    let output = tender(&root)
        .args(["wait", "--any", "any-fast", "any-slow"])
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    assert!(output.status.success());
    // Should return quickly (not wait for the 60s sleep).
    assert!(elapsed < std::time::Duration::from_secs(5));

    let stdout = String::from_utf8(output.stdout).unwrap();
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    // Only the terminal session(s) are in the output.
    assert!(!arr.is_empty());
    assert!(arr.iter().any(|m| m["session"] == "any-fast"));

    // Cleanup the slow session.
    tender(&root)
        .args(["kill", "--force", "any-slow"])
        .assert()
        .success();
}

#[test]
fn wait_mixed_exit_codes() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // One success, one failure.
    tender(&root)
        .args(["start", "mix-ok", "true"])
        .assert()
        .success();
    tender(&root)
        .args(["start", "mix-fail", "sh", "-c", "exit 1"])
        .assert()
        .success();
    wait_terminal(&root, "mix-ok");
    wait_terminal(&root, "mix-fail");

    tender(&root)
        .args(["wait", "mix-ok", "mix-fail"])
        .assert()
        .code(42);
}

#[test]
fn wait_single_session_emits_array_of_one() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "single-arr", "true"])
        .assert()
        .success();
    wait_terminal(&root, "single-arr");

    let output = tender(&root)
        .args(["wait", "single-arr"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["session"], "single-arr");
}

#[test]
fn wait_not_found_among_multiple() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "exists-ok", "true"])
        .assert()
        .success();
    wait_terminal(&root, "exists-ok");

    // One valid, one nonexistent -- should fail immediately.
    tender(&root)
        .args(["wait", "exists-ok", "does-not-exist"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn wait_multiple_timeout() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "mt-slow1", "sleep", "60"])
        .assert()
        .success();
    tender(&root)
        .args(["start", "mt-slow2", "sleep", "60"])
        .assert()
        .success();
    wait_running(&root, "mt-slow1");
    wait_running(&root, "mt-slow2");

    let start = std::time::Instant::now();
    tender(&root)
        .args(["wait", "--timeout", "1", "mt-slow1", "mt-slow2"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("timeout"));
    assert!(start.elapsed().as_secs() < 3);

    // Cleanup.
    tender(&root)
        .args(["kill", "--force", "mt-slow1"])
        .assert()
        .success();
    tender(&root)
        .args(["kill", "--force", "mt-slow2"])
        .assert()
        .success();
}

#[test]
fn wait_duplicate_session_names() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "dup-test", "true"])
        .assert()
        .success();
    wait_terminal(&root, "dup-test");

    // Passing the same name twice should emit one entry, not two.
    let output = tender(&root)
        .args(["wait", "dup-test", "dup-test"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(arr.len(), 1);
}

#[test]
fn wait_spawn_failed_beats_nonzero_exit() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // One non-zero exit, one spawn failure.
    tender(&root)
        .args(["start", "sev-exit", "sh", "-c", "exit 1"])
        .assert()
        .success();
    tender(&root)
        .args(["start", "sev-spawn", "/nonexistent/binary"])
        .output()
        .unwrap();
    wait_terminal(&root, "sev-exit");
    wait_terminal(&root, "sev-spawn");

    // Spawn failure (2) is more severe than non-zero exit (42).
    tender(&root)
        .args(["wait", "sev-exit", "sev-spawn"])
        .assert()
        .code(2);
}
