mod harness;

use harness::{tender, wait_running, wait_terminal};
use predicates::prelude::*;
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn start_same_spec_is_idempotent() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out1 = tender(&root)
        .args(["start", "idem-same", "sleep", "60"])
        .output()
        .unwrap();
    assert!(out1.status.success(), "first start failed");
    wait_running(&root, "idem-same");

    let meta1: serde_json::Value =
        serde_json::from_slice(&out1.stdout).expect("first output not JSON");
    let run_id1 = meta1["run_id"].as_str().expect("no run_id");

    let out2 = tender(&root)
        .args(["start", "idem-same", "sleep", "60"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "second start should succeed (idempotent)"
    );

    let meta2: serde_json::Value =
        serde_json::from_slice(&out2.stdout).expect("second output not JSON");
    let run_id2 = meta2["run_id"]
        .as_str()
        .expect("no run_id in second output");

    assert_eq!(
        run_id1, run_id2,
        "idempotent start should return same run_id"
    );

    tender(&root)
        .args(["kill", "--force", "idem-same"])
        .assert()
        .success();
}

#[test]
fn start_different_spec_is_conflict() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "idem-diff", "sleep", "60"])
        .assert()
        .success();
    wait_running(&root, "idem-diff");

    tender(&root)
        .args(["start", "idem-diff", "echo", "hi"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("session conflict"));

    tender(&root)
        .args(["kill", "--force", "idem-diff"])
        .assert()
        .success();
}

#[test]
fn start_after_terminal_is_error() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "idem-term", "true"])
        .assert()
        .success();
    wait_terminal(&root, "idem-term");

    tender(&root)
        .args(["start", "idem-term", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("terminal state"));
}

#[test]
fn start_with_cwd_child_runs_in_requested_directory() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();
    let work_dir = root.path().join("myworkdir");
    std::fs::create_dir_all(&work_dir).unwrap();

    let out = tender(&root)
        .args([
            "start",
            "cwd-test",
            "--cwd",
            work_dir.to_str().unwrap(),
            "--",
            "pwd",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_terminal(&root, "cwd-test");

    let log_out = tender(&root)
        .args(["log", "cwd-test", "--raw"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        log.contains(work_dir.to_str().unwrap()),
        "child should run in {work_dir:?}, got log: {log}"
    );
}

#[test]
fn start_with_env_child_sees_overridden_vars() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .args([
            "start",
            "env-test",
            "--env",
            "TENDER_TEST_VAR=hello_from_tender",
            "--",
            "sh",
            "-c",
            "echo $TENDER_TEST_VAR",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    wait_terminal(&root, "env-test");

    let log_out = tender(&root)
        .args(["log", "env-test", "--raw"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        log.contains("hello_from_tender"),
        "child should see TENDER_TEST_VAR, got log: {log}"
    );
}

#[test]
fn start_with_different_cwd_is_spec_conflict() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();
    let dir_a = root.path().join("dir_a");
    let dir_b = root.path().join("dir_b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let out1 = tender(&root)
        .args([
            "start",
            "cwd-conflict",
            "--cwd",
            dir_a.to_str().unwrap(),
            "--",
            "sleep",
            "60",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());

    let out2 = tender(&root)
        .args([
            "start",
            "cwd-conflict",
            "--cwd",
            dir_b.to_str().unwrap(),
            "--",
            "sleep",
            "60",
        ])
        .output()
        .unwrap();
    assert!(
        !out2.status.success(),
        "different cwd should be a spec conflict"
    );
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("session conflict"),
        "expected conflict error, got: {stderr}"
    );

    // Cleanup: kill the running session
    tender(&root)
        .args(["kill", "cwd-conflict", "--force"])
        .output()
        .unwrap();
    wait_terminal(&root, "cwd-conflict");
}

#[test]
fn start_with_different_env_is_spec_conflict() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out1 = tender(&root)
        .args([
            "start",
            "env-conflict",
            "--env",
            "FOO=bar",
            "--",
            "sleep",
            "60",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());

    let out2 = tender(&root)
        .args([
            "start",
            "env-conflict",
            "--env",
            "FOO=baz",
            "--",
            "sleep",
            "60",
        ])
        .output()
        .unwrap();
    assert!(
        !out2.status.success(),
        "different env should be a spec conflict"
    );
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("session conflict"),
        "expected conflict error, got: {stderr}"
    );

    // Cleanup: kill the running session
    tender(&root)
        .args(["kill", "env-conflict", "--force"])
        .output()
        .unwrap();
    wait_terminal(&root, "env-conflict");
}
