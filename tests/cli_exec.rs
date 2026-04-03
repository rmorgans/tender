mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// exec fails if session doesn't exist.
#[test]
fn exec_session_not_found() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["exec", "nonexistent", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session not found"));
}

/// exec fails if session is not running (terminal state).
#[test]
fn exec_session_not_running() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");
    harness::tender(&root)
        .args(["exec", "job1", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not running"));
}

/// Basic exec: run echo in a bash shell, get structured output.
#[test]
fn exec_basic_command() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start a bash shell with --stdin
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");

    // Give shell time to initialize
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Exec a command
    let output = harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "hello world"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"].as_str().unwrap().contains("hello world"));
    assert!(!result["timed_out"].as_bool().unwrap());
    assert!(result["cwd_after"].as_str().unwrap().starts_with('/'));

    let _ = harness::tender(&root)
        .args(["kill", "shell", "--force"])
        .assert();
}

/// exec fails if session lacks --stdin.
#[test]
fn exec_session_no_stdin() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");
    harness::tender(&root)
        .args(["exec", "job1", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--stdin"));
    let _ = harness::tender(&root)
        .args(["kill", "job1", "--force"])
        .assert();
}

/// exec propagates non-zero exit code; shell stays alive.
#[test]
fn exec_nonzero_exit() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args(["exec", "shell", "--", "false"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(1));

    // Shell still running after failed command
    let status_output = harness::tender(&root)
        .args(["status", "shell"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(status["status"].as_str(), Some("Running"));

    let _ = harness::tender(&root)
        .args(["kill", "shell", "--force"])
        .assert();
}

/// Shell state (cwd) persists across exec calls.
#[test]
fn exec_cwd_persists() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // cd to /tmp
    let output1 = harness::tender(&root)
        .args(["exec", "shell", "--", "cd", "/tmp"])
        .output()
        .unwrap();
    let result1: serde_json::Value = serde_json::from_slice(&output1.stdout).unwrap();
    // After cd, cwd_after should be /tmp (or /private/tmp on macOS)
    let cwd1 = result1["cwd_after"].as_str().unwrap();
    assert!(
        cwd1.contains("tmp"),
        "cwd_after should contain tmp, got: {cwd1}"
    );

    // Next exec should see /tmp as cwd
    let output2 = harness::tender(&root)
        .args(["exec", "shell", "--", "pwd"])
        .output()
        .unwrap();
    let result2: serde_json::Value = serde_json::from_slice(&output2.stdout).unwrap();
    assert!(result2["stdout"].as_str().unwrap().contains("tmp"));
    let cwd2 = result2["cwd_after"].as_str().unwrap();
    assert!(
        cwd2.contains("tmp"),
        "cwd_after should contain tmp, got: {cwd2}"
    );

    let _ = harness::tender(&root)
        .args(["kill", "shell", "--force"])
        .assert();
}

/// Annotation event is written to output.log after exec.
#[test]
fn exec_writes_annotation() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "annotated"])
        .assert()
        .success();

    let log_path = root
        .path()
        .join(".tender/sessions/default/shell/output.log");
    let content = std::fs::read_to_string(&log_path).unwrap();
    let ann_line: serde_json::Value = content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|line| line["tag"] == "A" && line["content"]["source"] == "agent.exec")
        .expect("annotation line should exist in output.log");
    let ann = &ann_line["content"];
    assert_eq!(ann["source"].as_str(), Some("agent.exec"));
    assert_eq!(ann["event"].as_str(), Some("exec"));
    assert_eq!(ann["data"]["hook_exit_code"].as_i64(), Some(0));
    assert!(ann["data"]["command"].is_array());

    let _ = harness::tender(&root)
        .args(["kill", "shell", "--force"])
        .assert();
}

/// exec --timeout: returns timeout error, shell stays alive.
#[test]
fn exec_timeout() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args(["exec", "shell", "--timeout", "1", "--", "sleep", "4"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(124));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["timed_out"].as_bool().unwrap());

    // Shell should still be running
    let status_output = harness::tender(&root)
        .args(["status", "shell"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(status["status"].as_str(), Some("Running"));

    let _ = harness::tender(&root)
        .args(["kill", "shell", "--force"])
        .assert();
}

/// Second concurrent exec fails with busy error.
#[test]
fn exec_concurrent_busy() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Start a long exec in the background
    let mut long_exec = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .env("HOME", root.path())
        .args(["exec", "shell", "--", "sleep", "30"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Give it time to acquire the lock
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Second exec should fail with busy
    harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "hello"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("another exec"));

    // Clean up
    let _ = long_exec.kill();
    let _ = long_exec.wait();
    let _ = harness::tender(&root)
        .args(["kill", "shell", "--force"])
        .assert();
}

/// exec with explicit --exec-target posix-shell.
#[test]
fn exec_explicit_posix_target() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--exec-target", "posix-shell", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "explicit"])
        .output()
        .unwrap();
    assert!(output.status.success(), "exec failed: {}", String::from_utf8_lossy(&output.stderr));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"].as_str().unwrap().contains("explicit"));

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}

/// exec on a session with no exec target fails with clear message.
#[test]
fn exec_none_target_rejected() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    // sleep is not a shell — infers ExecTarget::None
    harness::tender(&root)
        .args(["start", "sleeper", "--stdin", "--", "sleep", "60"])
        .assert()
        .success();
    harness::wait_running(&root, "sleeper");

    harness::tender(&root)
        .args(["exec", "sleeper", "--", "echo", "hello"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no exec target"));

    let _ = harness::tender(&root).args(["kill", "sleeper", "--force"]).assert();
}

/// bash infers PosixShell, exec works without --exec-target.
#[test]
fn exec_infers_posix_from_bash() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "inferred"])
        .output()
        .unwrap();
    assert!(output.status.success(), "exec failed: {}", String::from_utf8_lossy(&output.stderr));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["stdout"].as_str().unwrap().contains("inferred"));

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}

/// Invalid --exec-target value fails at start (clap rejects it).
#[test]
fn start_invalid_exec_target() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--exec-target", "fish", "--", "bash"])
        .assert()
        .failure();
}

/// Different --exec-target creates a session conflict (different spec hash).
#[test]
fn exec_target_changes_session_identity() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start with posix-shell
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--exec-target", "posix-shell", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");

    // Same name, different exec-target → conflict
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--exec-target", "powershell", "--", "bash"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session conflict"));

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}

/// Python REPL exec: basic print statement.
#[test]
fn exec_python_repl_basic() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "py", "--stdin", "--exec-target", "python-repl", "--", "python3", "-i"])
        .assert()
        .success();
    harness::wait_running(&root, "py");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let output = harness::tender(&root)
        .args(["exec", "py", "--timeout", "10", "--", "print('hello from python')"])
        .output()
        .unwrap();

    assert!(output.status.success(), "exec failed: {}", String::from_utf8_lossy(&output.stderr));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"].as_str().unwrap().contains("hello from python"));
    assert!(result["cwd_after"].as_str().unwrap().starts_with('/'));

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}

/// Python REPL exec: exception produces non-zero exit and traceback in stderr.
#[test]
fn exec_python_repl_exception() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "py", "--stdin", "--exec-target", "python-repl", "--", "python3", "-i"])
        .assert()
        .success();
    harness::wait_running(&root, "py");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let output = harness::tender(&root)
        .args(["exec", "py", "--timeout", "10", "--", "raise ValueError('boom')"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(1));
    assert!(result["stderr"].as_str().unwrap().contains("ValueError"));
    assert!(result["stderr"].as_str().unwrap().contains("boom"));

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}

/// Python REPL exec: cwd changes are tracked.
#[test]
fn exec_python_repl_cwd() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "py", "--stdin", "--exec-target", "python-repl", "--", "python3", "-i"])
        .assert()
        .success();
    harness::wait_running(&root, "py");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let output = harness::tender(&root)
        .args(["exec", "py", "--timeout", "10", "--", "import os; os.chdir('/tmp')"])
        .output()
        .unwrap();

    assert!(output.status.success(), "exec failed: {}", String::from_utf8_lossy(&output.stderr));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    // macOS /tmp is a symlink to /private/tmp
    assert!(
        result["cwd_after"].as_str().unwrap().contains("tmp"),
        "cwd should contain tmp, got: {}",
        result["cwd_after"]
    );

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}

/// python3 is NOT inferred as PythonRepl — requires explicit --exec-target.
/// Pipe mode needs `-i` flag, so inference would be misleading.
#[test]
fn exec_python_not_inferred() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "py", "--stdin", "--", "python3", "-i"])
        .assert()
        .success();
    harness::wait_running(&root, "py");

    // Without --exec-target, python3 infers None → exec rejected
    harness::tender(&root)
        .args(["exec", "py", "--", "print(1+1)"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no exec target"));

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}

/// Side-channel result file is cleaned up after exec.
#[test]
fn exec_python_result_file_cleaned() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "py", "--stdin", "--exec-target", "python-repl", "--", "python3", "-i"])
        .assert()
        .success();
    harness::wait_running(&root, "py");
    std::thread::sleep(std::time::Duration::from_millis(500));

    harness::tender(&root)
        .args(["exec", "py", "--timeout", "10", "--", "print('cleanup test')"])
        .assert()
        .success();

    // exec-results/ dir should exist but be empty (result file was cleaned up)
    let results_dir = root.path().join(".tender/sessions/default/py/exec-results");
    if results_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&results_dir).unwrap().collect();
        assert!(entries.is_empty(), "result files should be cleaned up, found: {entries:?}");
    }

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}
