// Remote SSH transport tests — POSIX only.
// All tests rely on fake shell scripts as ssh shims.
#![cfg(unix)]

mod harness;

use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

/// Helper: create a fake ssh script that dumps args to stdout.
/// Returns the TempDir (must be kept alive for PATH to stay valid).
fn fake_ssh_echo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nfor arg in \"$@\"; do echo \"ARG:$arg\"; done\n").unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();
    tmp
}

/// Helper: create a fake ssh that exits 0 immediately (no-op).
fn fake_ssh_noop() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();
    tmp
}

#[test]
fn host_flag_is_accepted_by_parser() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    // Use a fake ssh shim so we don't hit real SSH (ConnectTimeout).
    let tmp = fake_ssh_noop();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@example.com", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    // Should not fail with "unexpected argument '--host'"
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "parser should accept --host, got: {stderr}"
    );
}

#[test]
fn host_flag_invokes_ssh_with_correct_remote_command() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let args: Vec<&str> = stdout.lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .collect();
    // Should see: -T, -o, ConnectTimeout=10, user@box, tender, list
    assert!(args.contains(&"tender"), "should contain tender: {args:?}");
    assert!(args.contains(&"list"), "should contain list: {args:?}");
    // No "--" should be passed to ssh
    assert!(!args.contains(&"--"), "should not contain -- in ssh args: {args:?}");
}

/// Helper: extract the remote command from the fake ssh output.
///
/// `fake_ssh_echo()` prints each arg as `ARG:<value>`. The remote
/// command args are everything after the host (arg index 4+, since
/// the first 4 are: -T, -o, ConnectTimeout=10, <host>).
///
/// SSH concatenates these with spaces to form the remote command string.
/// We do the same and then `shell_words::split()` to simulate what the
/// remote POSIX shell would produce as argv.
fn parse_remote_argv(stdout: &str) -> Vec<String> {
    let args: Vec<&str> = stdout.lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .collect();
    // Skip: -T, -o, ConnectTimeout=10, <host>
    let remote_parts = &args[4..];
    let remote_cmd = remote_parts.join(" ");
    shell_words::split(&remote_cmd)
        .expect("remote command should be valid POSIX shell syntax")
}

// -- Task 4: Transport error classification --

#[test]
fn host_flag_exit_255_is_transport_error() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nexit 255\n").unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "transport error should exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("transport"),
        "stderr should mention transport failure, got: {stderr}"
    );
}

#[test]
fn host_flag_preserves_remote_exit_code() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nexit 42\n").unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "wait", "my-session"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(42), "should preserve remote exit code 42");
}

// -- Task 5: JSON output passthrough --

#[test]
fn host_flag_passes_through_json_stdout() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    let script = "#!/bin/sh\nprintf '{\"schema_version\":1,\"session\":\"remote-job\",\"status\":\"Running\"}\\n'\nexit 0\n";
    std::fs::write(&fake_ssh, script).unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "status", "remote-job"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("stdout should be valid JSON, got: {stdout}"));
    assert_eq!(parsed["session"], "remote-job");
    assert_eq!(parsed["status"], "Running");
    assert!(output.status.success());
}

// -- Task 6: NDJSON streaming passthrough --

#[test]
fn host_flag_passes_through_ndjson_stream() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    let script = r#"#!/bin/sh
echo '{"ts":1.0,"namespace":"default","session":"s1","run_id":"abc","source":"tender.sidecar","kind":"run","name":"run.started","data":{"status":"Running"}}'
echo '{"ts":2.0,"namespace":"default","session":"s1","run_id":"abc","source":"tender.sidecar","kind":"run","name":"run.exited","data":{"status":"Exited","reason":"ExitedOk","exit_code":0}}'
exit 0
"#;
    std::fs::write(&fake_ssh, script).unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "watch", "--events"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "should have 2 NDJSON lines, got: {stdout}");

    for line in &lines {
        let event: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|_| panic!("each line should be valid JSON: {line}"));
        assert_eq!(event["source"], "tender.sidecar");
    }

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["name"], "run.started");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["name"], "run.exited");
}

// -- Task 7: SSH spawn failure --

#[test]
fn host_flag_ssh_not_found_gives_clear_error() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "should fail when ssh not found");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ssh") || stderr.contains("spawn"),
        "error should mention ssh, got: {stderr}"
    );
}

// -- Task 8: Namespace forwarding --

#[test]
fn host_flag_forwards_namespace_and_strips_host() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "status", "my-session", "--namespace", "prod"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);
    assert!(parsed.contains(&"--namespace".to_string()), "should forward --namespace: {parsed:?}");
    assert!(parsed.contains(&"prod".to_string()), "should forward prod: {parsed:?}");
    assert!(!parsed.contains(&"--host".to_string()), "--host should NOT be in remote command: {parsed:?}");
}

// -- Task 9: Stderr passthrough --

#[test]
fn host_flag_passes_through_stderr() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    let script = "#!/bin/sh\necho 'session not found: oops' >&2\nexit 1\n";
    std::fs::write(&fake_ssh, script).unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "status", "oops"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found"),
        "remote stderr should pass through, got: {stderr}"
    );
    assert_eq!(output.status.code(), Some(1));
}

// -- Task 10: Trailing args, quoting, child --host --

#[test]
fn host_flag_forwards_start_with_trailing_args() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "start", "job", "--timeout", "30", "--", "sleep", "60"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);

    assert_eq!(parsed[0], "tender");
    assert!(parsed.contains(&"start".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"job".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"--timeout".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"30".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"sleep".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"60".to_string()), "parsed: {parsed:?}");
    assert!(!parsed.contains(&"--host".to_string()), "parsed: {parsed:?}");
}

#[test]
fn host_flag_quotes_child_args_with_spaces() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args([
            "--host", "user@box",
            "start", "job", "--",
            "echo", "hello world", "foo;bar", "$HOME",
        ])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);

    assert_eq!(parsed[0], "tender");
    assert!(parsed.contains(&"hello world".to_string()),
        "space-containing arg must survive round-trip: {parsed:?}");
    assert!(parsed.contains(&"foo;bar".to_string()),
        "semicolon-containing arg must survive: {parsed:?}");
    assert!(parsed.contains(&"$HOME".to_string()),
        "dollar sign must survive (not expanded): {parsed:?}");
}

#[test]
fn host_flag_does_not_eat_child_host_arg() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args([
            "--host", "user@box",
            "start", "job", "--",
            "myprog", "--host", "other-host",
        ])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);

    let host_indices: Vec<usize> = parsed.iter().enumerate()
        .filter(|(_, a)| a.as_str() == "--host")
        .map(|(i, _)| i)
        .collect();
    assert!(!host_indices.is_empty(),
        "child's --host must be preserved in remote command: {parsed:?}");
    for &i in &host_indices {
        if i + 1 < parsed.len() && parsed[i + 1] == "other-host" {
            return;
        }
    }
    panic!("child's --host other-host must be preserved: {parsed:?}");
}

// -- Task 9 (PTY): SSH -t for attach --

#[test]
fn host_flag_attach_uses_tty_allocation() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "attach", "my-session"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let args: Vec<&str> = stdout.lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .collect();
    assert!(args.contains(&"-t"), "attach should use -t for TTY: {args:?}");
    assert!(!args.contains(&"-T"), "attach should not use -T: {args:?}");
}

// -- Task 11: Allowlist rejection --

#[test]
fn host_flag_rejects_unsupported_commands() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    for cmd_args in &[
        vec!["--host", "user@box", "run", "script.sh"],
        vec!["--host", "user@box", "exec", "session", "--", "ls"],
        vec!["--host", "user@box", "wrap", "--source", "test.hook", "--event", "test", "--", "true"],
        vec!["--host", "user@box", "prune", "--all"],
    ] {
        let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
            .args(cmd_args)
            .output()
            .unwrap();

        assert!(
            !output.status.success(),
            "command {:?} should be rejected over SSH",
            cmd_args
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not supported"),
            "should mention 'not supported' for {:?}, got: {stderr}",
            cmd_args
        );
    }
}
