mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

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
