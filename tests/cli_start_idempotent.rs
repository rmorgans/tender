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
fn start_same_spec_is_idempotent() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // First start
    let out1 = run_tender(&root, &["start", "idem-same", "sleep", "60"]);
    assert!(out1.status.success(), "first start failed");
    wait_running(&root, "idem-same");

    let meta1: serde_json::Value =
        serde_json::from_slice(&out1.stdout).expect("first output not JSON");
    let run_id1 = meta1["run_id"].as_str().expect("no run_id");

    // Second start with exact same args
    let out2 = run_tender(&root, &["start", "idem-same", "sleep", "60"]);
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

    // Clean up
    run_tender(&root, &["kill", "--force", "idem-same"]);
}

#[test]
fn start_different_spec_is_conflict() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out1 = run_tender(&root, &["start", "idem-diff", "sleep", "60"]);
    assert!(out1.status.success(), "first start failed");
    wait_running(&root, "idem-diff");

    // Second start with different command
    let out2 = run_tender(&root, &["start", "idem-diff", "echo", "hi"]);
    assert!(
        !out2.status.success(),
        "second start with different spec should fail"
    );

    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("session conflict"),
        "stderr should mention conflict, got: {stderr}"
    );

    // Clean up
    run_tender(&root, &["kill", "--force", "idem-diff"]);
}

#[test]
fn start_after_terminal_is_error() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a command that exits immediately
    let out1 = run_tender(&root, &["start", "idem-term", "true"]);
    assert!(out1.status.success(), "first start failed");
    wait_terminal(&root, "idem-term");

    // Try to start again with same name
    let out2 = run_tender(&root, &["start", "idem-term", "true"]);
    assert!(!out2.status.success(), "start after terminal should fail");

    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr.contains("terminal state"),
        "stderr should mention terminal state, got: {stderr}"
    );
}
