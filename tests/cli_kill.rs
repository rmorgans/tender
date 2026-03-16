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

/// Wait for meta.json to show Running state on disk.
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
fn kill_running_process() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "kill-job", "sleep", "60"]);
    // Wait for meta.json to be written to disk (sidecar writes after pipe signal)
    wait_running(&root, "kill-job");

    let output = run_tender(&root, &["kill", "kill-job"]);
    assert!(output.status.success(), "kill failed");

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output is not JSON");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "Killed");
}

#[test]
fn kill_force() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "force-job", "sleep", "60"]);
    wait_running(&root, "force-job");

    let output = run_tender(&root, &["kill", "--force", "force-job"]);
    assert!(output.status.success());

    let meta = wait_terminal(&root, "force-job");
    // Force kill may show as Killed (SIGKILL detected as signal death)
    assert_eq!(meta["status"], "Exited");
}

#[test]
fn kill_already_dead_is_idempotent() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start and wait for natural exit
    run_tender(&root, &["start", "dead-job", "true"]);
    wait_terminal(&root, "dead-job");

    // Kill should succeed (idempotent)
    let output = run_tender(&root, &["kill", "dead-job"]);
    assert!(output.status.success());

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output is not JSON");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "ExitedOk");
}

#[test]
fn kill_nonexistent_session_is_idempotent() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let output = run_tender(&root, &["kill", "nope"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not_found"));
}

#[test]
fn kill_preserves_child_identity() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let start_out = run_tender(&root, &["start", "preserve-kill", "sleep", "60"]);
    let start_meta: serde_json::Value =
        serde_json::from_slice(&start_out.stdout).expect("not JSON");
    let start_pid = start_meta["child"]["pid"].as_u64().unwrap();
    wait_running(&root, "preserve-kill");

    let kill_out = run_tender(&root, &["kill", "preserve-kill"]);
    let kill_meta: serde_json::Value = serde_json::from_slice(&kill_out.stdout).expect("not JSON");
    let kill_pid = kill_meta["child"]["pid"].as_u64().unwrap();

    assert_eq!(start_pid, kill_pid);
}

#[test]
fn status_shows_killed_after_kill() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "status-kill", "sleep", "60"]);
    wait_running(&root, "status-kill");
    run_tender(&root, &["kill", "status-kill"]);

    let output = run_tender(&root, &["status", "status-kill"]);
    assert!(output.status.success());

    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).expect("not JSON");
    assert_eq!(meta["status"], "Exited");
    assert_eq!(meta["reason"], "Killed");
}
