use std::process::Command;
use tempfile::TempDir;

fn tender_bin() -> std::path::PathBuf {
    let status = Command::new("cargo")
        .args(["build", "--quiet"])
        .status()
        .expect("cargo build failed");
    assert!(status.success());
    let mut path = std::env::current_dir().unwrap();
    path.push("target/debug/tender");
    path
}

fn run_tender(root: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(tender_bin())
        .args(args)
        .env("HOME", root.path())
        .output()
        .expect("failed to run tender")
}

fn read_meta(root: &TempDir, session: &str) -> serde_json::Value {
    // Wait briefly for sidecar to write terminal state
    std::thread::sleep(std::time::Duration::from_millis(500));
    let path = root
        .path()
        .join(format!(".tender/sessions/{session}/meta.json"));
    let content = std::fs::read_to_string(&path).expect("meta.json not found");
    serde_json::from_str(&content).expect("invalid meta.json")
}

fn read_log(root: &TempDir, session: &str) -> String {
    std::thread::sleep(std::time::Duration::from_millis(500));
    let path = root
        .path()
        .join(format!(".tender/sessions/{session}/output.log"));
    std::fs::read_to_string(&path).unwrap_or_default()
}

// === Child spawn and supervision ===

#[test]
fn start_returns_running_with_child() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "echo-job", "echo", "hello"]);
    assert!(output.status.success());

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output is not JSON");
    assert_eq!(meta["status"], "Running");
    assert!(meta["child"]["pid"].is_number());
    assert!(meta["child"]["start_time_ns"].is_number());
}

#[test]
fn child_exit_ok_produces_exited_ok() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "ok-job", "true"]);
    assert!(output.status.success());

    let meta = read_meta(&root, "ok-job");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedOk");
    assert!(meta["ended_at"].is_string());
    assert!(meta["child"]["pid"].is_number());
}

#[test]
fn child_exit_error_produces_exited_error() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "err-job", "sh", "-c", "exit 42"]);
    assert!(output.status.success());

    let meta = read_meta(&root, "err-job");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedError");
    assert_eq!(meta["code"], 42);
}

#[test]
fn stdout_captured_to_output_log() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "stdout-job", "echo", "hello world"]);

    let log = read_log(&root, "stdout-job");
    assert!(log.contains("O hello world"), "log: {log}");

    // Each line has timestamp and tag
    for line in log.lines() {
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        assert!(parts.len() >= 3, "malformed log line: {line}");
        assert!(parts[0].contains('.'), "timestamp missing micros: {line}");
        assert!(parts[1] == "O" || parts[1] == "E", "bad tag: {}", parts[1]);
    }
}

#[test]
fn stderr_captured_to_output_log() {
    let root = TempDir::new().unwrap();
    run_tender(
        &root,
        &["start", "stderr-job", "sh", "-c", "echo error >&2"],
    );

    let log = read_log(&root, "stderr-job");
    assert!(log.contains("E error"), "log: {log}");
}

#[test]
fn interleaved_stdout_stderr() {
    let root = TempDir::new().unwrap();
    run_tender(
        &root,
        &[
            "start",
            "interleave-job",
            "sh",
            "-c",
            "echo out1; echo err1 >&2; echo out2",
        ],
    );

    let log = read_log(&root, "interleave-job");
    // All three lines should be present
    assert!(log.contains("O out1"), "missing out1 in: {log}");
    assert!(log.contains("E err1"), "missing err1 in: {log}");
    assert!(log.contains("O out2"), "missing out2 in: {log}");
}

#[test]
fn spawn_failure_produces_spawn_failed() {
    let root = TempDir::new().unwrap();
    let output = run_tender(
        &root,
        &["start", "bad-cmd", "nonexistent-command-xyz-12345"],
    );
    // CLI should still succeed (SpawnFailed is a valid durable state)
    assert!(output.status.success());

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output is not JSON");
    assert_eq!(meta["status"], "SpawnFailed");
}

#[test]
fn child_identity_preserved_in_terminal_state() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "preserve-job", "echo", "hi"]);
    assert!(output.status.success());

    let start_meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output is not JSON");
    let start_child_pid = start_meta["child"]["pid"].as_u64().unwrap();

    let final_meta = read_meta(&root, "preserve-job");
    let final_child_pid = final_meta["child"]["pid"].as_u64().unwrap();

    assert_eq!(start_child_pid, final_child_pid);
}

#[test]
fn lock_released_after_child_exits() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "lock-job", "echo", "hi"]);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let lock_path = root.path().join(".tender/sessions/lock-job/lock");
    if lock_path.exists() {
        use std::fs::File;
        use std::os::unix::io::AsRawFd;
        let file = File::open(&lock_path).unwrap();
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(ret, 0, "lock should be released after sidecar exits");
    }
}

#[test]
fn status_shows_terminal_after_child_exits() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "status-job", "echo", "hi"]);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let output = run_tender(&root, &["status", "status-job"]);
    assert!(output.status.success());

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output is not JSON");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedOk");
}
