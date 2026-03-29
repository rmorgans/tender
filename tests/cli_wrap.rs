mod harness;

use harness::{tender, wait_terminal};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

/// Start a session that runs `sleep 60`, wait for Running state.
fn create_running_session(root: &TempDir, name: &str, namespace: &str) {
    let out = tender(root)
        .args(["start", name, "--namespace", namespace, "--", "sleep", "60"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_running_ns(root, namespace, name);
}

/// Wait for meta.json to show Running state under a specific namespace.
fn wait_running_ns(root: &TempDir, namespace: &str, session: &str) {
    let path = root
        .path()
        .join(format!(".tender/sessions/{namespace}/{session}/meta.json"));
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
            panic!("timed out waiting for Running state in {namespace}/{session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Read the run_id from a session's meta.json.
fn read_run_id(root: &TempDir, namespace: &str, session: &str) -> String {
    let path = root
        .path()
        .join(format!(".tender/sessions/{namespace}/{session}/meta.json"));
    let content = std::fs::read_to_string(&path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
    meta["run_id"].as_str().unwrap().to_owned()
}

/// Read output.log and find A-tagged lines.
fn read_annotation_lines(root: &TempDir, namespace: &str, session: &str) -> Vec<String> {
    let path = root
        .path()
        .join(format!(".tender/sessions/{namespace}/{session}/output.log"));
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    content
        .lines()
        .filter(|line| {
            // Format: {ts} A {json}
            let parts: Vec<&str> = line.splitn(3, ' ').collect();
            parts.len() >= 2 && parts[1] == "A"
        })
        .map(|s| s.to_owned())
        .collect()
}

/// Extract the JSON payload from an A-tagged log line.
fn parse_annotation_json(line: &str) -> serde_json::Value {
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    assert!(parts.len() == 3, "expected 3 parts in annotation line");
    serde_json::from_str(parts[2]).expect("annotation payload should be valid JSON")
}

#[test]
fn wrap_child_inherits_tender_env() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // Start a session that prints its env
    let out = tender(&root)
        .args([
            "start",
            "wrap-env-test",
            "--namespace",
            "default",
            "--",
            "env",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    wait_terminal(&root, "wrap-env-test");

    let log_path = root
        .path()
        .join(".tender/sessions/default/wrap-env-test/output.log");
    let content = std::fs::read_to_string(&log_path).unwrap();

    assert!(
        content.contains("TENDER_RUN_ID="),
        "child should see TENDER_RUN_ID"
    );
    assert!(
        content.contains("TENDER_SESSION=wrap-env-test"),
        "child should see TENDER_SESSION"
    );
    assert!(
        content.contains("TENDER_NAMESPACE=default"),
        "child should see TENDER_NAMESPACE"
    );
}

#[test]
fn wrap_preserves_child_exit_code() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-exit", "default");
    let run_id = read_run_id(&root, "default", "wrap-exit");

    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-exit",
            "--source",
            "test.src",
            "--event",
            "test-event",
            "--",
            "sh",
            "-c",
            "exit 42",
        ])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(42),
        "wrap should exit with child's exit code"
    );

    // Cleanup
    tender(&root)
        .args(["kill", "--force", "wrap-exit"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-exit");
}

#[test]
fn wrap_captures_and_replays_stdout() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-stdout", "default");
    let run_id = read_run_id(&root, "default", "wrap-stdout");

    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-stdout",
            "--source",
            "test.src",
            "--event",
            "test-event",
            "--",
            "echo",
            "hello-from-wrap",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello-from-wrap"),
        "wrap stdout should contain child output, got: {stdout}"
    );

    // Also check annotation was written
    let ann_lines = read_annotation_lines(&root, "default", "wrap-stdout");
    assert_eq!(ann_lines.len(), 1, "should have one annotation line");
    let ann = parse_annotation_json(&ann_lines[0]);
    assert_eq!(ann["source"], "test.src");
    assert_eq!(ann["event"], "test-event");
    assert_eq!(ann["data"]["hook_exit_code"], 0);

    // Cleanup
    tender(&root)
        .args(["kill", "--force", "wrap-stdout"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-stdout");
}

#[test]
fn wrap_writes_annotation_line() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-ann", "default");
    let run_id = read_run_id(&root, "default", "wrap-ann");

    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-ann",
            "--source",
            "cmux.claude-hook",
            "--event",
            "pre-tool-use",
            "--",
            "echo",
            "ok",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let ann_lines = read_annotation_lines(&root, "default", "wrap-ann");
    assert_eq!(ann_lines.len(), 1);

    let ann = parse_annotation_json(&ann_lines[0]);
    assert_eq!(ann["source"], "cmux.claude-hook");
    assert_eq!(ann["event"], "pre-tool-use");
    assert_eq!(ann["run_id"], run_id);
    assert_eq!(ann["data"]["truncated"], false);

    // Cleanup
    tender(&root)
        .args(["kill", "--force", "wrap-ann"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-ann");
}

#[test]
fn wrap_fails_without_run_id() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // Don't set TENDER_RUN_ID
    let out = tender(&root)
        .env_remove("TENDER_RUN_ID")
        .args([
            "wrap",
            "--session",
            "nonexistent",
            "--source",
            "test.src",
            "--event",
            "test",
            "--",
            "echo",
            "hi",
        ])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "wrap should fail without TENDER_RUN_ID"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("TENDER_RUN_ID"),
        "error should mention TENDER_RUN_ID, got: {stderr}"
    );
}

#[test]
fn wrap_fails_for_nonexistent_session() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .env("TENDER_RUN_ID", "fake-run-id")
        .args([
            "wrap",
            "--session",
            "no-such-session",
            "--source",
            "test.src",
            "--event",
            "test",
            "--",
            "echo",
            "hi",
        ])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "wrap should fail for nonexistent session"
    );
}

#[test]
fn source_rejects_tender_prefix() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .env("TENDER_RUN_ID", "fake-run-id")
        .args([
            "wrap",
            "--session",
            "any",
            "--source",
            "tender.sidecar",
            "--event",
            "test",
            "--",
            "echo",
            "hi",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success(), "wrap should reject tender.* source");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("tender."),
        "error should mention reserved prefix, got: {stderr}"
    );
}

#[test]
fn wrap_defaults_session_from_env() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-env-default", "myns");
    let run_id = read_run_id(&root, "myns", "wrap-env-default");

    // Don't pass --session or --namespace, rely on env vars
    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .env("TENDER_SESSION", "wrap-env-default")
        .env("TENDER_NAMESPACE", "myns")
        .args([
            "wrap",
            "--source",
            "test.src",
            "--event",
            "env-default",
            "--",
            "echo",
            "from-env",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "wrap should succeed with env defaults: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ann_lines = read_annotation_lines(&root, "myns", "wrap-env-default");
    assert_eq!(ann_lines.len(), 1, "annotation should exist");

    // Cleanup
    tender(&root)
        .args(["kill", "--force", "wrap-env-default", "--namespace", "myns"])
        .output()
        .unwrap();
    let path = root
        .path()
        .join(".tender/sessions/myns/wrap-env-default/meta.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                let status = meta["status"].as_str().unwrap_or("");
                if status != "Starting" && status != "Running" {
                    break;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn wrap_truncates_large_payload() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-trunc", "default");
    let run_id = read_run_id(&root, "default", "wrap-trunc");

    // Generate a large stdin payload (>4096 bytes)
    let large_input = "x".repeat(5000);

    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-trunc",
            "--source",
            "test.src",
            "--event",
            "big-event",
            "--",
            "cat",
        ])
        .write_stdin(large_input.as_bytes())
        .output()
        .unwrap();
    assert!(out.status.success());

    let ann_lines = read_annotation_lines(&root, "default", "wrap-trunc");
    assert_eq!(ann_lines.len(), 1);

    // Verify line is within limit
    let line = &ann_lines[0];
    assert!(
        line.len() <= 4096,
        "annotation line should be ≤4096 bytes, got {}",
        line.len()
    );

    let ann = parse_annotation_json(line);
    assert_eq!(ann["data"]["truncated"], true, "should be marked truncated");

    // Cleanup
    tender(&root)
        .args(["kill", "--force", "wrap-trunc"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-trunc");
}

#[test]
fn wrap_passes_stdin_to_child() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-stdin", "default");
    let run_id = read_run_id(&root, "default", "wrap-stdin");

    let input = r#"{"tool":"bash","input":"ls"}"#;

    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-stdin",
            "--source",
            "test.src",
            "--event",
            "stdin-test",
            "--",
            "cat",
        ])
        .write_stdin(input.as_bytes())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        input,
        "child should receive and echo the stdin blob"
    );

    // Verify annotation recorded the stdin
    let ann_lines = read_annotation_lines(&root, "default", "wrap-stdin");
    assert_eq!(ann_lines.len(), 1);
    let ann = parse_annotation_json(&ann_lines[0]);
    // hook_stdin should be parsed as JSON since input is valid JSON
    assert_eq!(ann["data"]["hook_stdin"]["tool"], "bash");

    // Cleanup
    tender(&root)
        .args(["kill", "--force", "wrap-stdin"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-stdin");
}

#[test]
fn wrap_annotation_visible_in_watch() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-watch", "default");
    let run_id = read_run_id(&root, "default", "wrap-watch");

    let bin = assert_cmd::cargo::cargo_bin("tender");
    let mut watch_child = Command::new(bin)
        .args([
            "watch",
            "--namespace",
            "default",
            "--annotations",
            "--from-now",
        ])
        .env("HOME", root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn tender watch");

    std::thread::sleep(std::time::Duration::from_millis(300));

    let out = tender(&root)
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-watch",
            "--source",
            "cmux.claude-hook",
            "--event",
            "pre-tool-use",
            "--",
            "echo",
            "watch-visible",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    std::thread::sleep(std::time::Duration::from_secs(1));
    let _ = watch_child.kill();
    let output = watch_child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let saw_annotation = stdout.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .map(|event| {
                event["kind"] == "annotation"
                    && event["session"] == "wrap-watch"
                    && event["source"] == "cmux.claude-hook"
                    && event["name"] == "annotation.pre-tool-use"
                    && event["data"]["event"] == "pre-tool-use"
            })
            .unwrap_or(false)
    });
    assert!(
        saw_annotation,
        "watch --annotations should emit the wrap annotation, got:\n{stdout}"
    );

    tender(&root)
        .args(["kill", "--force", "wrap-watch"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-watch");
}

#[cfg(unix)]
#[test]
fn wrap_forwards_sigterm_and_writes_annotation() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    create_running_session(&root, "wrap-sigterm", "default");
    let run_id = read_run_id(&root, "default", "wrap-sigterm");

    let bin = assert_cmd::cargo::cargo_bin("tender");
    let mut wrap_child = Command::new(bin)
        .env("HOME", root.path())
        .env("TENDER_RUN_ID", &run_id)
        .args([
            "wrap",
            "--session",
            "wrap-sigterm",
            "--source",
            "test.src",
            "--event",
            "sigterm-test",
            "--",
            "sh",
            "-c",
            "trap 'exit 0' TERM; while :; do sleep 1; done",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn tender wrap");

    std::thread::sleep(std::time::Duration::from_millis(300));
    unsafe {
        libc::kill(wrap_child.id() as i32, libc::SIGTERM);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let status = loop {
        if let Some(status) = wrap_child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = wrap_child.kill();
            panic!("wrap did not exit after SIGTERM");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(
        status.success(),
        "wrap should exit cleanly after forwarding SIGTERM, got: {status:?}"
    );

    let ann_lines = read_annotation_lines(&root, "default", "wrap-sigterm");
    assert_eq!(ann_lines.len(), 1, "should write an annotation on SIGTERM");
    let ann = parse_annotation_json(&ann_lines[0]);
    assert_eq!(ann["event"], "sigterm-test");
    assert_eq!(ann["data"]["hook_exit_code"], 0);

    tender(&root)
        .args(["kill", "--force", "wrap-sigterm"])
        .output()
        .unwrap();
    wait_terminal(&root, "wrap-sigterm");
}
