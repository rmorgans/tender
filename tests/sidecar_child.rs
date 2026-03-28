// These tests spawn real processes (CLI -> sidecar -> child).
mod harness;

use harness::{tender, wait_terminal};
use predicates::prelude::*;
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

fn read_log(root: &TempDir, session: &str) -> String {
    wait_terminal(root, session);
    let path = root
        .path()
        .join(format!(".tender/sessions/default/{session}/output.log"));
    std::fs::read_to_string(&path).unwrap_or_default()
}

#[test]
fn start_returns_running_with_child() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let output = tender(&root)
        .args(["start", "echo-job", "echo", "hello"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(meta["status"], "Running");
    assert!(meta["child"]["pid"].is_number());
    assert!(meta["child"]["start_time_ns"].is_number());
}

#[test]
fn child_exit_ok_produces_exited_ok() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "ok-job", "true"])
        .assert()
        .success();

    let meta = wait_terminal(&root, "ok-job");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedOk");
    assert!(meta["ended_at"].is_string());
    assert!(meta["child"]["pid"].is_number());
}

#[test]
fn child_exit_error_produces_exited_error() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "err-job", "sh", "-c", "exit 42"])
        .assert()
        .success();

    let meta = wait_terminal(&root, "err-job");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedError");
    assert_eq!(meta["code"], 42);
}

#[test]
fn stdout_captured_to_output_log() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "stdout-job", "echo", "hello world"])
        .assert()
        .success();

    let log = read_log(&root, "stdout-job");
    assert!(log.contains("O hello world"), "log: {log}");

    for line in log.lines() {
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        assert!(parts.len() >= 3, "malformed log line: {line}");
        assert!(parts[0].contains('.'), "timestamp missing micros: {line}");
        assert!(parts[1] == "O" || parts[1] == "E", "bad tag: {}", parts[1]);
    }
}

#[test]
fn stderr_captured_to_output_log() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "stderr-job", "sh", "-c", "echo error >&2"])
        .assert()
        .success();

    let log = read_log(&root, "stderr-job");
    assert!(log.contains("E error"), "log: {log}");
}

#[test]
fn interleaved_stdout_stderr() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args([
            "start",
            "interleave-job",
            "sh",
            "-c",
            "echo out1; echo err1 >&2; echo out2",
        ])
        .assert()
        .success();

    let log = read_log(&root, "interleave-job");
    assert!(log.contains("O out1"), "missing out1 in: {log}");
    assert!(log.contains("E err1"), "missing err1 in: {log}");
    assert!(log.contains("O out2"), "missing out2 in: {log}");
}

#[test]
fn spawn_failure_produces_spawn_failed() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let output = tender(&root)
        .args(["start", "bad-cmd", "nonexistent-command-xyz-12345"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(meta["status"], "SpawnFailed");
}

#[test]
fn child_identity_preserved_in_terminal_state() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let output = tender(&root)
        .args(["start", "preserve-job", "echo", "hi"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let start_meta: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let start_pid = start_meta["child"]["pid"].as_u64().unwrap();

    let final_meta = wait_terminal(&root, "preserve-job");
    let final_pid = final_meta["child"]["pid"].as_u64().unwrap();
    assert_eq!(start_pid, final_pid);
}

#[test]
fn lock_released_after_child_exits() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "lock-job", "echo", "hi"])
        .assert()
        .success();
    wait_terminal(&root, "lock-job");

    #[cfg(unix)]
    {
        let lock_path = root.path().join(".tender/sessions/default/lock-job/lock");
        if lock_path.exists() {
            use std::fs::File;
            use std::os::unix::io::AsRawFd;
            let file = File::open(&lock_path).unwrap();
            let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            assert_eq!(ret, 0, "lock should be released after sidecar exits");
        }
    }
}

#[test]
fn status_shows_terminal_after_child_exits() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "status-job", "echo", "hi"])
        .assert()
        .success();
    wait_terminal(&root, "status-job");

    tender(&root)
        .args(["status", "status-job"])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""status": "Exited"#))
        .stdout(predicate::str::contains(r#""reason": "ExitedOk"#));
}
