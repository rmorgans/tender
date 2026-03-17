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

/// Read meta.json from disk and return parsed JSON.
fn read_meta_json(root: &TempDir, session: &str) -> serde_json::Value {
    let path = root
        .path()
        .join(format!(".tender/sessions/{session}/meta.json"));
    let content = std::fs::read_to_string(&path).expect("failed to read meta.json");
    serde_json::from_str(&content).expect("failed to parse meta.json")
}

/// Get the sidecar PID from meta JSON.
fn sidecar_pid(meta: &serde_json::Value) -> i32 {
    meta["sidecar"]["pid"]
        .as_u64()
        .expect("sidecar.pid not found") as i32
}

/// Wait for a process to actually die (not just be signaled).
fn wait_pid_dead(pid: i32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let ret = unsafe { libc::kill(pid, 0) };
        if ret != 0 {
            return; // Process is gone
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for pid {pid} to die");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn status_reconciles_crashed_sidecar() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a long-running session
    run_tender(&root, &["start", "crashed-sc", "sleep", "60"]);
    wait_running(&root, "crashed-sc");

    // Get sidecar PID and kill it directly (simulating a crash)
    let meta = read_meta_json(&root, "crashed-sc");
    let sc_pid = sidecar_pid(&meta);
    unsafe {
        libc::kill(sc_pid, libc::SIGKILL);
    }
    wait_pid_dead(sc_pid);

    // Now status should reconcile to SidecarLost
    let output = run_tender(&root, &["status", "crashed-sc"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: serde_json::Value = serde_json::from_str(&stdout).expect("output is not JSON");
    assert_eq!(result["status"], "SidecarLost");

    // Clean up: kill the orphaned child
    if let Some(child_pid) = result["child"]["pid"].as_u64() {
        unsafe {
            libc::kill(child_pid as i32, libc::SIGKILL);
        }
    }
}

#[test]
fn status_does_not_reconcile_running_session() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "still-running", "sleep", "60"]);
    wait_running(&root, "still-running");

    // Status should show Running (sidecar holds lock)
    let output = run_tender(&root, &["status", "still-running"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: serde_json::Value = serde_json::from_str(&stdout).expect("output is not JSON");
    assert_eq!(result["status"], "Running");

    // Clean up
    run_tender(&root, &["kill", "--force", "still-running"]);
}

#[test]
fn wait_reconciles_crashed_sidecar() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "wait-crashed", "sleep", "60"]);
    wait_running(&root, "wait-crashed");

    // Kill the sidecar directly
    let meta = read_meta_json(&root, "wait-crashed");
    let sc_pid = sidecar_pid(&meta);
    unsafe {
        libc::kill(sc_pid, libc::SIGKILL);
    }
    wait_pid_dead(sc_pid);

    // Wait should reconcile and return SidecarLost with exit code 3
    let output = run_tender(&root, &["wait", "--timeout", "5", "wait-crashed"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "SidecarLost should produce exit code 3"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: serde_json::Value = serde_json::from_str(&stdout).expect("output is not JSON");
    assert_eq!(result["status"], "SidecarLost");

    // Clean up: kill the orphaned child
    if let Some(child_pid) = result["child"]["pid"].as_u64() {
        unsafe {
            libc::kill(child_pid as i32, libc::SIGKILL);
        }
    }
}
