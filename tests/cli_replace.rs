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
fn replace_running_session() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a long-running session
    let out1 = run_tender(&root, &["start", "repl-run", "sleep", "60"]);
    assert!(out1.status.success(), "first start failed");
    wait_running(&root, "repl-run");

    let meta1: serde_json::Value =
        serde_json::from_slice(&out1.stdout).expect("first output not JSON");
    let run_id1 = meta1["run_id"].as_str().expect("no run_id").to_string();

    // Replace with a different command
    let out2 = run_tender(&root, &["start", "--replace", "repl-run", "sleep", "60"]);
    assert!(out2.status.success(), "replace start failed");

    let meta2: serde_json::Value =
        serde_json::from_slice(&out2.stdout).expect("second output not JSON");
    let run_id2 = meta2["run_id"].as_str().expect("no run_id in second output");

    // Should be a NEW session (different run_id)
    assert_ne!(run_id1, run_id2, "replace should create a new run_id");

    // Clean up
    run_tender(&root, &["kill", "--force", "repl-run"]);
}

#[test]
fn replace_terminal_session() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start and wait for natural exit
    let out1 = run_tender(&root, &["start", "repl-term", "true"]);
    assert!(out1.status.success(), "first start failed");
    wait_terminal(&root, "repl-term");

    // Replace with a long-running session
    let out2 = run_tender(&root, &["start", "--replace", "repl-term", "sleep", "60"]);
    assert!(out2.status.success(), "replace after terminal failed");

    let meta2: serde_json::Value =
        serde_json::from_slice(&out2.stdout).expect("second output not JSON");
    assert!(
        meta2["run_id"].as_str().is_some(),
        "replace should produce a new session with run_id"
    );

    // Clean up
    run_tender(&root, &["kill", "--force", "repl-term"]);
}

#[test]
fn replace_nonexistent_is_noop() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // --replace on a name that doesn't exist should work like normal start
    let out = run_tender(&root, &["start", "--replace", "repl-nope", "true"]);
    assert!(
        out.status.success(),
        "replace on nonexistent session should succeed"
    );

    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("output not JSON");
    assert!(meta["run_id"].as_str().is_some(), "should have a run_id");
}
