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
fn log_shows_child_output() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "log-echo", "echo", "hello from child"]);
    wait_terminal(&root, "log-echo");

    let output = run_tender(&root, &["log", "log-echo"]);
    assert!(output.status.success(), "log command failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello from child"),
        "expected 'hello from child' in stdout: {stdout}"
    );
}

#[test]
fn log_tail() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(
        &root,
        &[
            "start",
            "log-tail",
            "sh",
            "-c",
            "echo line1; echo line2; echo line3",
        ],
    );
    wait_terminal(&root, "log-tail");

    let output = run_tender(&root, &["log", "--tail", "1", "log-tail"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("line3"),
        "expected 'line3' in stdout: {stdout}"
    );
    assert!(
        !stdout.contains("line1"),
        "should not contain 'line1': {stdout}"
    );
}

#[test]
fn log_grep() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(
        &root,
        &[
            "start",
            "log-grep",
            "sh",
            "-c",
            "echo good; echo bad; echo good again",
        ],
    );
    wait_terminal(&root, "log-grep");

    let output = run_tender(&root, &["log", "--grep", "good", "log-grep"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("good"),
        "expected 'good' in stdout: {stdout}"
    );
    assert!(
        stdout.contains("good again"),
        "expected 'good again' in stdout: {stdout}"
    );
    assert!(
        !stdout.contains("bad"),
        "should not contain 'bad': {stdout}"
    );
}

#[test]
fn log_raw_strips_prefix() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "log-raw", "echo", "just content"]);
    wait_terminal(&root, "log-raw");

    let output = run_tender(&root, &["log", "--raw", "log-raw"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("just content"),
        "expected 'just content' in stdout: {stdout}"
    );
    // Raw mode should not have timestamp prefixes (digits.digits pattern)
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        assert!(
            !line.contains(" O ") && !line.contains(" E "),
            "raw line should not contain stream tag: {line}"
        );
    }
}

#[test]
fn log_nonexistent_session_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let output = run_tender(&root, &["log", "nope"]);
    assert!(
        !output.status.success(),
        "expected non-zero exit for nonexistent session"
    );
}

#[test]
fn log_no_output_file_returns_empty() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a binary that doesn't exist — SpawnFailed, no output.log created
    let start_out = run_tender(&root, &["start", "nolog-test", "/nonexistent/binary"]);
    // start exits 2 for SpawnFailed, that's expected
    assert_eq!(
        start_out.status.code(),
        Some(2),
        "expected exit code 2 for SpawnFailed"
    );

    wait_terminal(&root, "nolog-test");

    let output = run_tender(&root, &["log", "nolog-test"]);
    assert!(
        output.status.success(),
        "log should succeed for session with no output.log"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.is_empty(), "expected empty stdout, got: {stdout}");
}

#[test]
fn log_stderr_captured() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    run_tender(&root, &["start", "log-stderr", "sh", "-c", "echo err >&2"]);
    wait_terminal(&root, "log-stderr");

    let output = run_tender(&root, &["log", "log-stderr"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("err"), "expected 'err' in stdout: {stdout}");
    // Verify it has the E tag (stderr marker)
    assert!(
        stdout.contains(" E "),
        "expected stderr tag ' E ' in output: {stdout}"
    );
}

#[test]
fn log_since_filters_by_time() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a job that produces output, wait for it to finish
    run_tender(
        &root,
        &[
            "start",
            "log-since",
            "sh",
            "-c",
            "echo early; sleep 1; echo late",
        ],
    );
    wait_terminal(&root, "log-since");

    // --since 1s should only show lines from the last second.
    // "late" was written ~0s ago, "early" was written ~1-2s ago.
    // Use a generous window: --since 2s should get both, --since 0s should get none.
    // The reliable test: full log has 2 lines, --since with a future epoch has 0.
    let full = run_tender(&root, &["log", "log-since"]);
    let full_stdout = String::from_utf8_lossy(&full.stdout);
    assert!(
        full_stdout.contains("early") && full_stdout.contains("late"),
        "full log should have both lines: {full_stdout}"
    );

    // Use epoch far in the future — should return 0 lines
    let output = run_tender(&root, &["log", "--since", "9999999999", "log-since"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "since far-future should return nothing: {stdout}"
    );
}

#[test]
fn log_follow_stops_on_terminal_session() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    // Start a short-lived job
    run_tender(&root, &["start", "log-follow", "echo", "follow me"]);
    wait_terminal(&root, "log-follow");

    // --follow --tail 10 on an already-terminal session should read existing output
    // and exit (not block forever). Without --tail, follow seeks to EOF and shows
    // only new lines — which for a terminal session is nothing.
    let start = std::time::Instant::now();
    let output = run_tender(&root, &["log", "--follow", "--tail", "10", "log-follow"]);
    let elapsed = start.elapsed();

    assert!(output.status.success());
    assert!(
        elapsed.as_secs() < 5,
        "follow on terminal session blocked for {elapsed:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("follow me"),
        "follow should show existing output: {stdout}"
    );
}
