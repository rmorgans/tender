//! End-to-end acceptance criteria from docs/plans/active/01_event-emit-primitive.md.
//! Companion coverage: cli_events_lifecycle.rs (WAL crash injection),
//! cli_events_reconcile.rs (healing), cli_emit.rs (exit codes, lost+found),
//! events_log.rs (2×1000 concurrent writers at the append layer).

mod harness;

use harness::{tender, wait_running, wait_terminal};
use tempfile::TempDir;

fn parse_ndjson(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("NDJSON line parses"))
        .collect()
}

/// Criterion 1: `kill -9` a supervised run; `tender events` replays
/// `run.starting → run.started → run.sidecar_lost` with occurrence-time
/// timestamps and `provenance:"inferred"` on the last.
#[cfg(unix)]
#[test]
fn kill_nine_replays_full_lifecycle_via_events() {
    let root = TempDir::new().unwrap();
    tender(&root)
        .args(["start", "s1", "--", "sleep", "60"])
        .assert()
        .success();
    wait_running(&root, "s1");

    let meta_path = root.path().join(".tender/sessions/default/s1/meta.json");
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    let sc_pid = meta["sidecar"]["pid"].as_u64().unwrap() as i32;
    unsafe { libc::kill(sc_pid, libc::SIGKILL) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while unsafe { libc::kill(sc_pid, 0) } == 0 {
        assert!(std::time::Instant::now() < deadline, "sidecar never died");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Reconciliation runs on the next status/wait touch ("after reboot" in
    // the criterion = any later CLI contact).
    let status_out = tender(&root).args(["status", "s1"]).output().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status_out.stdout).unwrap();

    let output = tender(&root)
        .args(["events", "--session", "default/s1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let events = parse_ndjson(&output.stdout);
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["run.starting", "run.started", "run.sidecar_lost"]);

    // Occurrence-time stamps: fixed-width RFC 3339 µs Z, non-decreasing.
    let stamps: Vec<&str> = events.iter().map(|e| e["ts"].as_str().unwrap()).collect();
    assert!(stamps.iter().all(|t| t.len() == 27 && t.ends_with('Z')));
    assert!(stamps.windows(2).all(|w| w[0] <= w[1]));

    assert_eq!(events[2]["data"]["provenance"], "inferred");

    if let Some(child_pid) = status["child"]["pid"].as_u64() {
        unsafe { libc::kill(child_pid as i32, libc::SIGKILL) };
    }
}

/// Criterion 3 (process-level; the 2×1000 writer test lives in
/// events_log.rs): concurrent `tender emit` processes interleave without
/// torn lines and every event survives.
#[test]
fn concurrent_emit_processes_no_torn_lines() {
    let root = TempDir::new().unwrap();
    tender(&root)
        .args(["start", "s1", "--", "echo", "hi"])
        .assert()
        .success();
    wait_terminal(&root, "s1");

    let bin = assert_cmd::cargo::cargo_bin("tender");
    let home = root.path().to_path_buf();
    std::thread::scope(|scope| {
        for t in 0..2 {
            let bin = bin.clone();
            let home = home.clone();
            scope.spawn(move || {
                for i in 0..50 {
                    let status = std::process::Command::new(&bin)
                        .env("HOME", &home)
                        .args([
                            "emit",
                            "--kind",
                            "stress.emit",
                            "--session",
                            "s1",
                            "--data",
                            &format!(r#"{{"t":{t},"i":{i}}}"#),
                        ])
                        .status()
                        .expect("emit process runs");
                    assert!(status.success());
                }
            });
        }
    });

    // Every line in every segment must parse — a torn line would not.
    let events_dir = root.path().join(".tender/sessions/default/s1/events");
    let mut stress = 0;
    for entry in std::fs::read_dir(&events_dir)
        .unwrap()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let event: serde_json::Value =
                serde_json::from_str(line).expect("no torn or interleaved lines");
            if event["kind"] == "stress.emit" {
                stress += 1;
            }
        }
    }
    assert_eq!(stress, 100, "all 100 emitted events present");
}

/// Criterion 4: an emit with 1 MiB data produces a blob + preview event with
/// `truncated:true` and a valid `data_ref.sha256`; identical payload emitted
/// twice stores one blob.
#[test]
fn one_mib_emit_spills_to_deduped_blob() {
    let root = TempDir::new().unwrap();
    tender(&root)
        .args(["start", "s1", "--", "echo", "hi"])
        .assert()
        .success();
    wait_terminal(&root, "s1");

    let payload = format!(r#"{{"blob":"{}"}}"#, "x".repeat(1024 * 1024));
    let data_file = root.path().join("payload.json");
    std::fs::write(&data_file, &payload).unwrap();

    for _ in 0..2 {
        tender(&root)
            .args([
                "emit",
                "--kind",
                "bulk.payload",
                "--session",
                "s1",
                "--data-file",
                data_file.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    let output = tender(&root)
        .args(["events", "--kind", "bulk."])
        .output()
        .unwrap();
    let events = parse_ndjson(&output.stdout);
    assert_eq!(events.len(), 2);

    let session_dir = root.path().join(".tender/sessions/default/s1");
    for event in &events {
        assert_eq!(event["truncated"], true);
        let data_ref = &event["data_ref"];
        let sha = data_ref["sha256"].as_str().unwrap();
        assert_eq!(sha.len(), 64);
        assert_eq!(
            data_ref["path"].as_str().unwrap(),
            format!("events/blobs/{sha}")
        );

        // The blob is the full payload, addressed by its hash.
        let blob_path = session_dir.join(data_ref["path"].as_str().unwrap());
        let blob = std::fs::read(&blob_path).unwrap();
        assert_eq!(blob.len() as u64, data_ref["bytes"].as_u64().unwrap());
        let full: serde_json::Value = serde_json::from_slice(&blob).unwrap();
        let original: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(full, original);

        // Preview is small and marked.
        let preview = serde_json::to_string(&event["data"]).unwrap();
        assert!(preview.len() <= 4 * 1024 + 64);
    }

    // Content-addressing dedupes the identical payload.
    let blob_count = std::fs::read_dir(session_dir.join("events/blobs"))
        .unwrap()
        .count();
    assert_eq!(blob_count, 1, "identical payloads store one blob");

    // Cross-check the hash with the system tool when available (macOS/Linux).
    let sha = events[0]["data_ref"]["sha256"].as_str().unwrap();
    let blob_path = session_dir.join(format!("events/blobs/{sha}"));
    if let Ok(out) = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(&blob_path)
        .output()
    {
        if out.status.success() {
            let system_sha = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap()
                .to_owned();
            assert_eq!(system_sha, sha, "sha256 matches the system tool");
        }
    }
}

/// Criterion 6: DuckDB reads the segments with zero schema wrangling.
/// Self-skips when duckdb is not installed.
#[test]
fn duckdb_reads_events_as_typed_rows() {
    if std::process::Command::new("duckdb")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: duckdb not on PATH");
        return;
    }

    let root = TempDir::new().unwrap();
    tender(&root)
        .args(["start", "s1", "--", "echo", "hi"])
        .assert()
        .success();
    wait_terminal(&root, "s1");

    let glob = root
        .path()
        .join(".tender/sessions/default/s1/events/*.jsonl");
    let query = format!(
        "SELECT kind, seq, run_id, ts FROM read_json('{}') ORDER BY seq",
        glob.display()
    );
    let out = std::process::Command::new("duckdb")
        .args(["-json", "-c", &query])
        .output()
        .expect("duckdb runs");
    assert!(
        out.status.success(),
        "duckdb failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows.len(), 3);
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["run.starting", "run.started", "run.exited"]);
    assert_eq!(rows[0]["seq"], 1);
    assert!(rows[0]["run_id"].is_string());
}
