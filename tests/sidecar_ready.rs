use std::process::Command;
use tempfile::TempDir;

fn tender_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_tender"))
}

fn run_tender(root: &TempDir, args: &[&str]) -> std::process::Output {
    let bin = tender_bin();
    Command::new(bin)
        .args(args)
        .env("HOME", root.path())
        .output()
        .expect("failed to run tender")
}

fn run_tender_status(output: &std::process::Output) -> (i32, String, String) {
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (code, stdout, stderr)
}

#[test]
fn start_returns_promptly_not_blocked_by_child() {
    let root = TempDir::new().unwrap();

    let start = std::time::Instant::now();
    let output = run_tender(&root, &["start", "prompt-test", "sleep", "60"]);
    let elapsed = start.elapsed();

    assert!(output.status.success(), "start failed");
    // The readiness handshake must complete without waiting for the child.
    // If the ready pipe fd leaks to the child, start blocks until child exits (60s).
    assert!(
        elapsed.as_secs() < 5,
        "tender start blocked for {elapsed:?} — ready pipe fd likely leaked to child"
    );

    // Clean up: kill the child so it doesn't linger
    let _ = run_tender(&root, &["kill", "--force", "prompt-test"]);
}

#[test]
fn start_creates_session_and_returns_json() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "test-job", "echo", "hello"]);
    let (code, stdout, stderr) = run_tender_status(&output);

    assert_eq!(code, 0, "start failed: {stderr}");

    // Parse output as JSON
    let meta: serde_json::Value = serde_json::from_str(&stdout).expect("output is not valid JSON");

    assert_eq!(meta["session"], "test-job");
    assert_eq!(meta["schema_version"], 1);
    // Sidecar spawns child and signals Running
    assert_eq!(meta["status"], "Running");
    assert!(meta["run_id"].is_string());
    assert!(meta["sidecar"]["pid"].is_number());
    assert!(meta["sidecar"]["start_time_ns"].is_number());
    assert!(meta["child"]["pid"].is_number());
    assert_eq!(meta["launch_spec"]["argv"][0], "echo");
    assert_eq!(meta["launch_spec"]["argv"][1], "hello");
}

#[test]
fn start_writes_durable_meta_json() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "durable-test", "echo", "hi"]);
    assert!(output.status.success());

    // meta.json should exist on disk
    let meta_path = root.path().join(".tender/sessions/durable-test/meta.json");
    assert!(meta_path.exists(), "meta.json not written to disk");

    let content = std::fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(meta["session"], "durable-test");
}

#[test]
fn start_same_name_after_completed_fails_already_exists() {
    let root = TempDir::new().unwrap();

    // First start succeeds
    let output1 = run_tender(&root, &["start", "dup-test", "echo", "a"]);
    assert!(output1.status.success());

    // Second start with same name fails (AlreadyExists)
    let output2 = run_tender(&root, &["start", "dup-test", "echo", "b"]);
    let (code, _, stderr) = run_tender_status(&output2);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("already exists"),
        "expected 'already exists' error, got: {stderr}"
    );
}

#[test]
fn status_reads_session() {
    let root = TempDir::new().unwrap();

    // Create a session
    let output = run_tender(&root, &["start", "status-test", "echo", "hi"]);
    assert!(output.status.success());

    // Read it back with status
    let output = run_tender(&root, &["status", "status-test"]);
    let (code, stdout, stderr) = run_tender_status(&output);
    assert_eq!(code, 0, "status failed: {stderr}");

    let meta: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(meta["session"], "status-test");
}

#[test]
fn status_nonexistent_fails() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["status", "nope"]);
    assert!(!output.status.success());
}

#[test]
fn list_shows_sessions() {
    let root = TempDir::new().unwrap();

    // Empty list
    let output = run_tender(&root, &["list"]);
    let (code, stdout, _) = run_tender_status(&output);
    assert_eq!(code, 0);
    let names: Vec<String> = serde_json::from_str(&stdout).unwrap();
    assert!(names.is_empty());

    // Create sessions
    run_tender(&root, &["start", "bravo", "echo", "b"]);
    run_tender(&root, &["start", "alpha", "echo", "a"]);

    // List should show both, sorted
    let output = run_tender(&root, &["list"]);
    let (code, stdout, _) = run_tender_status(&output);
    assert_eq!(code, 0);
    let names: Vec<String> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(names, vec!["alpha", "bravo"]);
}

#[test]
fn launch_spec_json_cleaned_up() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "cleanup-test", "echo", "hi"]);
    assert!(output.status.success());

    // launch_spec.json should have been deleted by sidecar
    let spec_path = root
        .path()
        .join(".tender/sessions/cleanup-test/launch_spec.json");
    assert!(
        !spec_path.exists(),
        "launch_spec.json should be cleaned up after sidecar reads it"
    );
}

#[test]
fn lock_released_after_sidecar_exits() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["start", "lock-test", "echo", "hi"]);
    assert!(output.status.success());

    // Wait for sidecar to finish supervising the child and exit
    std::thread::sleep(std::time::Duration::from_millis(500));

    let lock_path = root.path().join(".tender/sessions/lock-test/lock");

    if lock_path.exists() {
        use std::fs::File;
        use std::os::unix::io::AsRawFd;

        let file = File::open(&lock_path).unwrap();
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(ret, 0, "lock should be released after sidecar exits");
    }
}
