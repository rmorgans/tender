use std::process::Command;
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

fn tender_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_tender"))
}

fn run_tender(root: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(tender_bin())
        .args(args)
        .env("HOME", root.path())
        .output()
        .expect("failed to run tender")
}

fn wait_running(root: &TempDir, session: &str) {
    let path = root
        .path()
        .join(format!(".tender/sessions/{session}/meta.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                if meta["status"].as_str() == Some("Running") {
                    return;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for Running state in {session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn wait_terminal(root: &TempDir, session: &str) -> serde_json::Value {
    let path = root
        .path()
        .join(format!(".tender/sessions/{session}/meta.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                let status = meta["status"].as_str().unwrap_or("");
                if status != "Starting" && status != "Running" {
                    return meta;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for terminal state in {session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn wait_returns_terminal_state() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "wait-ok", "true"]);
    wait_terminal(&root, "wait-ok");

    let output = run_tender(&root, &["wait", "wait-ok"]);
    assert!(output.status.success(), "wait should exit 0 for ExitedOk");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let meta: serde_json::Value = serde_json::from_str(&stdout).expect("output is not JSON");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedOk");
}

#[test]
fn wait_blocks_until_exit() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "wait-block", "sleep", "2"]);
    wait_running(&root, "wait-block");

    let start = std::time::Instant::now();
    let output = run_tender(&root, &["wait", "wait-block"]);
    let elapsed = start.elapsed();

    assert!(output.status.success());
    assert!(
        elapsed > std::time::Duration::from_secs(1),
        "should block at least 1s, got {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "should not take more than 5s, got {elapsed:?}"
    );
}

#[test]
fn wait_timeout_expires() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "wait-timeout", "sleep", "60"]);
    wait_running(&root, "wait-timeout");

    let start = std::time::Instant::now();
    let output = run_tender(&root, &["wait", "--timeout", "1", "wait-timeout"]);
    let elapsed = start.elapsed();

    assert!(!output.status.success(), "wait should fail on timeout");
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "timeout should be quick, got {elapsed:?}"
    );

    // Clean up: kill the sleep
    run_tender(&root, &["kill", "--force", "wait-timeout"]);
}

#[test]
fn wait_nonexistent_session_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let output = run_tender(&root, &["wait", "nope"]);
    assert!(!output.status.success());
}

#[test]
fn wait_exit_code_42_for_nonzero_child() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "wait-err", "sh", "-c", "exit 3"]);
    wait_terminal(&root, "wait-err");

    let output = run_tender(&root, &["wait", "wait-err"]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "non-zero child exit should produce exit code 42"
    );
}

#[test]
fn wait_exit_code_2_for_spawn_failed() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "wait-spawn", "/nonexistent/binary"]);
    // SpawnFailed is written by sidecar, wait for it
    wait_terminal(&root, "wait-spawn");

    let output = run_tender(&root, &["wait", "wait-spawn"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "spawn failure should produce exit code 2"
    );
}
