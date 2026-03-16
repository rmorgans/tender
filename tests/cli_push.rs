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

fn run_tender_stdin(root: &TempDir, args: &[&str], input: &[u8]) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(tender_bin())
        .args(args)
        .env("HOME", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn tender");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
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
fn push_delivers_stdin_to_child() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "--stdin", "push-echo", "cat"]);
    wait_running(&root, "push-echo");

    let push_out = run_tender_stdin(&root, &["push", "push-echo"], b"hello from push\n");
    assert!(
        push_out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    run_tender(&root, &["kill", "push-echo"]);
    wait_terminal(&root, "push-echo");

    let log_out = run_tender(&root, &["log", "push-echo"]);
    assert!(log_out.status.success());
    let stdout = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        stdout.contains("hello from push"),
        "expected 'hello from push' in log output: {stdout}"
    );
}

#[test]
fn push_multiple_sequential() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "--stdin", "push-multi", "cat"]);
    wait_running(&root, "push-multi");

    let out1 = run_tender_stdin(&root, &["push", "push-multi"], b"first line\n");
    assert!(
        out1.status.success(),
        "first push failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    let out2 = run_tender_stdin(&root, &["push", "push-multi"], b"second line\n");
    assert!(
        out2.status.success(),
        "second push failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    run_tender(&root, &["kill", "push-multi"]);
    wait_terminal(&root, "push-multi");

    let log_out = run_tender(&root, &["log", "push-multi"]);
    assert!(log_out.status.success());
    let stdout = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        stdout.contains("first line"),
        "expected 'first line' in log output: {stdout}"
    );
    assert!(
        stdout.contains("second line"),
        "expected 'second line' in log output: {stdout}"
    );
}

#[test]
fn push_to_session_without_stdin_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "nostdin-job", "sleep", "60"]);
    wait_running(&root, "nostdin-job");

    let push_out = run_tender_stdin(&root, &["push", "nostdin-job"], b"nope\n");
    assert!(
        !push_out.status.success(),
        "push should fail without --stdin"
    );
    let stderr = String::from_utf8_lossy(&push_out.stderr);
    assert!(
        stderr.contains("not started with --stdin"),
        "expected '--stdin' error in stderr: {stderr}"
    );

    // Clean up the long-running process
    run_tender(&root, &["kill", "nostdin-job"]);
    wait_terminal(&root, "nostdin-job");
}

#[test]
fn push_to_nonexistent_session_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let push_out = run_tender_stdin(&root, &["push", "nope"], b"data\n");
    assert!(
        !push_out.status.success(),
        "push to nonexistent session should fail"
    );
}

#[test]
fn push_to_terminal_session_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "--stdin", "term-push", "true"]);
    wait_terminal(&root, "term-push");

    let push_out = run_tender_stdin(&root, &["push", "term-push"], b"too late\n");
    assert!(
        !push_out.status.success(),
        "push to terminal session should fail"
    );
    let stderr = String::from_utf8_lossy(&push_out.stderr);
    assert!(
        stderr.contains("not running"),
        "expected 'not running' error in stderr: {stderr}"
    );
}

#[test]
fn push_immediately_after_start() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // start returns only after sidecar signals readiness (FIFO already created)
    // so push should work without wait_running
    run_tender(&root, &["start", "--stdin", "imm-push", "cat"]);

    let push_out = run_tender_stdin(&root, &["push", "imm-push"], b"immediate data\n");
    assert!(
        push_out.status.success(),
        "immediate push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    run_tender(&root, &["kill", "imm-push"]);
    wait_terminal(&root, "imm-push");

    let log_out = run_tender(&root, &["log", "imm-push"]);
    assert!(log_out.status.success());
    let stdout = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        stdout.contains("immediate data"),
        "expected 'immediate data' in log output: {stdout}"
    );
}
