mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Returns the Python interpreter argv for the current platform.
/// POSIX: `["python3", "-i"]`, Windows: `["py", "-3", "-i"]`.
fn python_repl_argv() -> &'static [&'static str] {
    if cfg!(windows) {
        &["py", "-3", "-i"]
    } else {
        &["python3", "-i"]
    }
}

/// Build tender start args for a Python REPL session.
fn python_start_args(session: &str) -> Vec<String> {
    let mut args: Vec<String> = ["start", session, "--stdin", "--exec-target", "python-repl", "--"]
        .iter().map(|s| s.to_string()).collect();
    args.extend(python_repl_argv().iter().map(|s| s.to_string()));
    args
}

/// Build tender start args for a Python session without --exec-target (for inference tests).
fn python_start_args_no_target(session: &str) -> Vec<String> {
    let mut args: Vec<String> = ["start", session, "--stdin", "--"]
        .iter().map(|s| s.to_string()).collect();
    args.extend(python_repl_argv().iter().map(|s| s.to_string()));
    args
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
    let cwd = result["cwd_after"].as_str().unwrap();
    // Git Bash on Windows returns MSYS paths like /c/Users/... which
    // Path::is_absolute() doesn't recognise on Windows. Accept both styles.
    assert!(
        std::path::Path::new(cwd).is_absolute() || cwd.starts_with('/'),
        "cwd_after should be absolute, got: {cwd}"
    );

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
        .args(python_start_args("py"))
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
    let cwd = result["cwd_after"].as_str().unwrap();
    assert!(
        std::path::Path::new(cwd).is_absolute(),
        "cwd_after should be absolute, got: {cwd}"
    );

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}

/// Python REPL exec: exception produces non-zero exit and traceback in stderr.
#[test]
fn exec_python_repl_exception() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(python_start_args("py"))
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
        .args(python_start_args("py"))
        .assert()
        .success();
    harness::wait_running(&root, "py");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let tmp = std::env::temp_dir();
    let tmp_str = tmp.to_str().expect("temp dir should be valid UTF-8");
    // Use forward slashes — valid on Windows and avoids raw string edge cases
    let chdir_code = format!("import os; os.chdir('{}')", tmp_str.replace('\\', "/"));
    let output = harness::tender(&root)
        .args(["exec", "py", "--timeout", "10", "--", &chdir_code])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "exec failed:\nstderr: {}\nstdout: {}\ncode: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
        chdir_code,
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    let cwd = result["cwd_after"].as_str().unwrap();
    assert!(
        std::path::Path::new(cwd).is_absolute(),
        "cwd should be absolute after chdir, got: {cwd}"
    );

    let _ = harness::tender(&root).args(["kill", "py", "--force"]).assert();
}

/// python/python3 is NOT inferred as PythonRepl — requires explicit --exec-target.
/// Pipe mode needs `-i` flag, so inference would be misleading.
#[test]
fn exec_python_not_inferred() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(python_start_args_no_target("py"))
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

/// DuckDB inferred from argv[0].
#[test]
fn exec_infers_duckdb() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "db", "--stdin", "--", "duckdb"])
        .assert()
        .success();
    harness::wait_running(&root, "db");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Verify the session was created with DuckDb exec target
    let status_output = harness::tender(&root)
        .args(["status", "db"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(
        status["launch_spec"]["exec_target"].as_str(),
        Some("DuckDb")
    );

    let _ = harness::tender(&root)
        .args(["kill", "db", "--force"])
        .assert();
}

/// DuckDB exec: basic SELECT query returns structured JSON in stdout.
#[test]
fn exec_duckdb_basic_select() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args([
            "start", "db", "--stdin", "--exec-target", "duckdb", "--", "duckdb",
        ])
        .assert()
        .success();
    harness::wait_running(&root, "db");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args([
            "exec", "db", "--timeout", "10", "--", "SELECT 42 as answer;",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(!result["timed_out"].as_bool().unwrap());
    assert_eq!(result["cwd_after"].as_str(), Some("."));

    // Query results flow through stdout (captured from the output log)
    let stdout = result["stdout"].as_str().expect("stdout should be present");
    assert!(
        stdout.contains("42"),
        "stdout should contain query result with 42, got: {stdout}"
    );

    // No exec-results directory — DuckDB doesn't use side-channel files
    let results_dir = root
        .path()
        .join(".tender/sessions/default/db/exec-results");
    assert!(
        !results_dir.exists(),
        "exec-results/ should not exist for DuckDB"
    );

    let _ = harness::tender(&root)
        .args(["kill", "db", "--force"])
        .assert();
}

/// DuckDB exec: SQL error reports exit_code 1 but keeps the session alive.
#[test]
fn exec_duckdb_sql_error() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args([
            "start", "db", "--stdin", "--exec-target", "duckdb", "--", "duckdb",
        ])
        .assert()
        .success();
    harness::wait_running(&root, "db");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Invalid SQL — error goes to stderr, sentinel still fires, exit_code = 1.
    let output = harness::tender(&root)
        .args([
            "exec",
            "db",
            "--timeout",
            "10",
            "--",
            "SELECT * FROM nonexistent_table_xyz;",
        ])
        .output()
        .unwrap();

    // tender exec mirrors the inner exit code — exits 1 on SQL error.
    // JSON result is still printed to stdout.
    assert_eq!(
        output.status.code(),
        Some(1),
        "tender exec should exit 1 (mirroring SQL error)"
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("stdout should contain JSON result even on error");
    assert_eq!(result["exit_code"].as_i64(), Some(1));
    assert!(!result["timed_out"].as_bool().unwrap());
    let stderr = result["stderr"].as_str().unwrap_or("");
    assert!(
        stderr.contains("nonexistent_table_xyz"),
        "stderr should contain the error table name, got: {stderr}"
    );

    // Session should still be running — errors don't kill DuckDB
    harness::tender(&root)
        .args(["status", "db"])
        .assert()
        .success();

    // Verify the session can still handle another query after error
    let output2 = harness::tender(&root)
        .args([
            "exec", "db", "--timeout", "10", "--", "SELECT 'recovered' as status;",
        ])
        .output()
        .unwrap();
    assert!(output2.status.success());
    let result2: serde_json::Value = serde_json::from_slice(&output2.stdout).unwrap();
    assert_eq!(result2["exit_code"].as_i64(), Some(0));
    let stdout2 = result2["stdout"].as_str().unwrap_or("");
    assert!(
        stdout2.contains("recovered"),
        "second query should succeed after error: {stdout2}"
    );

    let _ = harness::tender(&root)
        .args(["kill", "db", "--force"])
        .assert();
}

/// DuckDB exec: multiple statements produce concatenated results.
#[test]
fn exec_duckdb_multi_statement() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args([
            "start", "db", "--stdin", "--exec-target", "duckdb", "--", "duckdb",
        ])
        .assert()
        .success();
    harness::wait_running(&root, "db");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args([
            "exec",
            "db",
            "--timeout",
            "10",
            "--",
            "SELECT 1 as a;\nSELECT 2 as b;",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));

    // Both query results should be in stdout
    let stdout = result["stdout"].as_str().expect("stdout should be present");
    assert!(stdout.contains('1'), "stdout should contain first query result");
    assert!(stdout.contains('2'), "stdout should contain second query result");

    let _ = harness::tender(&root)
        .args(["kill", "db", "--force"])
        .assert();
}

/// DuckDB exec with explicit --exec-target duckdb.
#[test]
fn exec_duckdb_explicit_target() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args([
            "start", "db", "--stdin", "--exec-target", "duckdb", "--", "duckdb",
        ])
        .assert()
        .success();
    harness::wait_running(&root, "db");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args([
            "exec",
            "db",
            "--timeout",
            "10",
            "--",
            "SELECT 'hello' as greeting;",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));

    let _ = harness::tender(&root)
        .args(["kill", "db", "--force"])
        .assert();
}

/// DuckDB exec: mixed success — first statement succeeds, second fails.
/// Must report exit_code 1 even though stdout has partial results.
#[test]
fn exec_duckdb_mixed_success_reports_error() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args([
            "start", "db", "--stdin", "--exec-target", "duckdb", "--", "duckdb",
        ])
        .assert()
        .success();
    harness::wait_running(&root, "db");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args([
            "exec",
            "db",
            "--timeout",
            "10",
            "--",
            "SELECT 1 as ok;\nSELECT * FROM nonexistent_table_xyz;",
        ])
        .output()
        .unwrap();

    let result: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("stdout should contain JSON result");
    assert_eq!(
        result["exit_code"].as_i64(),
        Some(1),
        "mixed success should report exit_code 1"
    );
    // stdout should still have the first query's results
    let stdout = result["stdout"].as_str().unwrap_or("");
    assert!(stdout.contains('1'), "stdout should contain first query result: {stdout}");
    // stderr should have the error from the second query
    let stderr = result["stderr"].as_str().unwrap_or("");
    assert!(
        stderr.contains("nonexistent_table_xyz"),
        "stderr should contain the error: {stderr}"
    );

    // Session should still be alive
    harness::tender(&root)
        .args(["status", "db"])
        .assert()
        .success();

    let _ = harness::tender(&root)
        .args(["kill", "db", "--force"])
        .assert();
}

/// DuckDB exec: paths with spaces work correctly (no .output path escaping needed).
#[test]
fn exec_duckdb_path_with_spaces() {
    let _lock = lock();
    // Create a temp dir whose path includes spaces
    let parent = tempfile::TempDir::new().unwrap();
    let spaced_dir = parent.path().join("path with spaces");
    std::fs::create_dir_all(&spaced_dir).unwrap();

    // Use the spaced directory as HOME so session paths contain spaces
    let mut cmd = assert_cmd::Command::cargo_bin("tender").unwrap();
    cmd.env("HOME", &spaced_dir);
    cmd.args([
        "start", "db", "--stdin", "--exec-target", "duckdb", "--", "duckdb",
    ]);
    cmd.assert().success();

    // Wait for running
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut status_cmd = assert_cmd::Command::cargo_bin("tender").unwrap();
        status_cmd.env("HOME", &spaced_dir);
        status_cmd.args(["status", "db"]);
        let out = status_cmd.output().unwrap();
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("Running") {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for db to start");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));

    let mut exec_cmd = assert_cmd::Command::cargo_bin("tender").unwrap();
    exec_cmd.env("HOME", &spaced_dir);
    exec_cmd.args([
        "exec", "db", "--timeout", "10", "--", "SELECT 42 as answer;",
    ]);
    let output = exec_cmd.output().unwrap();

    assert!(
        output.status.success(),
        "exec with spaces in path failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    let stdout = result["stdout"].as_str().unwrap_or("");
    assert!(
        stdout.contains("42"),
        "stdout should contain query result: {stdout}"
    );

    let mut kill_cmd = assert_cmd::Command::cargo_bin("tender").unwrap();
    kill_cmd.env("HOME", &spaced_dir);
    kill_cmd.args(["kill", "db", "--force"]);
    let _ = kill_cmd.output();
}

/// Side-channel result file is cleaned up after exec.
#[test]
fn exec_python_result_file_cleaned() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(python_start_args("py"))
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
