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
fn timeout_kills_child() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let start = std::time::Instant::now();
    run_tender(
        &root,
        &["start", "--timeout", "2", "timeout-job", "sleep", "60"],
    );
    wait_running(&root, "timeout-job");

    let meta = wait_terminal(&root, "timeout-job");
    let elapsed = start.elapsed();

    assert_eq!(meta["status"], "Exited", "should reach Exited state");
    assert_eq!(meta["reason"], "TimedOut", "reason should be TimedOut");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "should finish within 5s, got {elapsed:?}"
    );
}

#[test]
fn timeout_not_triggered() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(
        &root,
        &["start", "--timeout", "60", "fast-job", "true"],
    );

    let meta = wait_terminal(&root, "fast-job");

    assert_eq!(meta["status"], "Exited");
    assert_eq!(
        meta["reason"], "ExitedOk",
        "child exiting before timeout should produce ExitedOk, not TimedOut"
    );
}
