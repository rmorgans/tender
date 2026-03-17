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
