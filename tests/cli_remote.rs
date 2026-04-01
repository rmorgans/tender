mod harness;

use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

/// Helper: create a fake ssh script that dumps args to stdout.
/// Returns the TempDir (must be kept alive for PATH to stay valid).
#[cfg(unix)]
fn fake_ssh_echo() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nfor arg in \"$@\"; do echo \"ARG:$arg\"; done\n").unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();
    tmp
}

#[test]
fn host_flag_is_accepted_by_parser() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@example.com", "list"])
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
