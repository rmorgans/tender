mod harness;

use harness::{tender, wait_running, wait_terminal};
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn replace_running_session() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out1 = tender(&root)
        .args(["start", "repl-run", "sleep", "60"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    wait_running(&root, "repl-run");

    let meta1: serde_json::Value = serde_json::from_slice(&out1.stdout).unwrap();
    let run_id1 = meta1["run_id"].as_str().unwrap().to_string();

    let out2 = tender(&root)
        .args(["start", "--replace", "repl-run", "sleep", "60"])
        .output()
        .unwrap();
    assert!(out2.status.success());

    let meta2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    let run_id2 = meta2["run_id"].as_str().unwrap();

    assert_ne!(run_id1, run_id2, "replace should create a new run_id");

    tender(&root)
        .args(["kill", "--force", "repl-run"])
        .assert()
        .success();
}

#[test]
fn replace_terminal_session() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "repl-term", "true"])
        .assert()
        .success();
    wait_terminal(&root, "repl-term");

    let out2 = tender(&root)
        .args(["start", "--replace", "repl-term", "sleep", "60"])
        .output()
        .unwrap();
    assert!(out2.status.success());

    let meta2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert!(meta2["run_id"].as_str().is_some());

    tender(&root)
        .args(["kill", "--force", "repl-term"])
        .assert()
        .success();
}

#[test]
fn replace_nonexistent_is_noop() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .args(["start", "--replace", "repl-nope", "true"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(meta["run_id"].as_str().is_some());
}

#[test]
fn replace_increments_generation() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out1 = tender(&root)
        .args(["start", "gen-test", "sleep", "60"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let meta1: serde_json::Value = serde_json::from_slice(&out1.stdout).unwrap();
    assert_eq!(meta1["generation"], 1);

    let out2 = tender(&root)
        .args(["start", "--replace", "gen-test", "sleep", "60"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let meta2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(meta2["generation"], 2);

    tender(&root)
        .args(["kill", "--force", "gen-test"])
        .assert()
        .success();
}
