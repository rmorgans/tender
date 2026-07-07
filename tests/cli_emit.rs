//! `tender emit` — spec §6 (write surface) and §7 (orphan emitters).

mod harness;

use harness::{tender, wait_terminal};
use tempfile::TempDir;

/// Read all events for a session, merged by (ts, writer, seq).
fn read_events(root: &TempDir, session: &str) -> Vec<serde_json::Value> {
    let events_dir = root
        .path()
        .join(format!(".tender/sessions/default/{session}/events"));
    let mut segments: Vec<_> = std::fs::read_dir(&events_dir)
        .expect("events dir exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    segments.sort();

    let mut events: Vec<serde_json::Value> = Vec::new();
    for seg in segments {
        for line in std::fs::read_to_string(&seg).unwrap().lines() {
            if !line.is_empty() {
                events.push(serde_json::from_str(line).expect("event line parses"));
            }
        }
    }
    events.sort_by_key(|e| {
        (
            e["ts"].as_str().unwrap().to_owned(),
            e["writer"].as_str().unwrap().to_owned(),
            e["seq"].as_u64().unwrap(),
        )
    });
    events
}

/// Start a session running `echo hi`, wait for it to finish, return run_id.
fn finished_session(root: &TempDir, name: &str) -> String {
    tender(root)
        .args(["start", name, "--", "echo", "hi"])
        .assert()
        .success();
    let meta = wait_terminal(root, name);
    meta["run_id"].as_str().unwrap().to_owned()
}

/// The plan's canonical validation example: a hook emitting with env context.
#[test]
fn emit_with_env_context_appends_event() {
    let root = TempDir::new().unwrap();
    let run_id = finished_session(&root, "s1");

    tender(&root)
        .env("TENDER_SESSION", "s1")
        .env("TENDER_NAMESPACE", "default")
        .env("TENDER_RUN_ID", &run_id)
        .env("TENDER_GENERATION", "1")
        .args([
            "emit",
            "--kind",
            "hook.post_tool_use",
            "--source",
            "claude.hook",
            "--data-stdin",
            "--best-effort",
        ])
        .write_stdin(r#"{"tool_name":"Bash"}"#)
        .assert()
        .success();

    let events = read_events(&root, "s1");
    let hook = events
        .iter()
        .find(|e| e["kind"] == "hook.post_tool_use")
        .expect("hook event appended");
    assert_eq!(hook["run_id"].as_str().unwrap(), run_id);
    assert_eq!(hook["source"], "claude.hook");
    assert_eq!(hook["gen"], 1);
    assert_eq!(hook["seq"], 1, "CLI emitter starts its own seq at 1");
    assert_ne!(hook["writer"].as_str().unwrap(), run_id, "fresh writer id");
    assert_eq!(hook["data"]["tool_name"], "Bash");
    assert_eq!(hook["v"], 1);
}

#[test]
fn emit_with_explicit_session_resolves_run_id_from_meta() {
    let root = TempDir::new().unwrap();
    let run_id = finished_session(&root, "s1");

    tender(&root)
        .args([
            "emit",
            "--kind",
            "build.finished",
            "--session",
            "default/s1",
            "--data",
            r#"{"ok":true,"artifacts":3}"#,
        ])
        .assert()
        .success();

    let events = read_events(&root, "s1");
    let event = events
        .iter()
        .find(|e| e["kind"] == "build.finished")
        .expect("event appended");
    assert_eq!(event["run_id"].as_str().unwrap(), run_id);
    assert_eq!(event["source"], "user.emit", "source defaults to user.emit");
    assert_eq!(event["data"]["artifacts"], 3);
}

#[test]
fn emit_bare_session_name_defaults_namespace() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    tender(&root)
        .args(["emit", "--kind", "ci.done", "--session", "s1"])
        .assert()
        .success();

    let events = read_events(&root, "s1");
    let event = events.iter().find(|e| e["kind"] == "ci.done").unwrap();
    assert_eq!(event["namespace"], "default");
    assert!(event.get("data").is_none(), "no data flag → no data field");
}

#[test]
fn emit_parent_flag_sets_parent_id() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    let parent = uuid::Uuid::now_v7().to_string();
    tender(&root)
        .args([
            "emit",
            "--kind",
            "ci.step",
            "--session",
            "s1",
            "--parent",
            &parent,
        ])
        .assert()
        .success();

    let events = read_events(&root, "s1");
    let event = events.iter().find(|e| e["kind"] == "ci.step").unwrap();
    assert_eq!(event["parent_id"].as_str().unwrap(), parent);
}

// --- Exit codes (spec §6): 0 ok, 2 usage, 3 no context, 5 not found, 6 invalid kind/source ---

#[test]
fn emit_reserved_kind_exits_6() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    for kind in ["run.custom", "tender.x", "exec.thing"] {
        tender(&root)
            .args(["emit", "--kind", kind, "--session", "s1"])
            .assert()
            .code(6);
    }
    // hook. is deliberately unreserved.
    tender(&root)
        .args(["emit", "--kind", "hook.custom", "--session", "s1"])
        .assert()
        .success();
}

#[test]
fn emit_invalid_kind_grammar_exits_6() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    tender(&root)
        .args(["emit", "--kind", "nodot", "--session", "s1"])
        .assert()
        .code(6);
}

#[test]
fn emit_reserved_source_exits_6() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    tender(&root)
        .args([
            "emit",
            "--kind",
            "ci.done",
            "--source",
            "tender.fake",
            "--session",
            "s1",
        ])
        .assert()
        .code(6);
}

#[test]
fn emit_without_context_exits_3() {
    let root = TempDir::new().unwrap();
    tender(&root)
        .env_remove("TENDER_SESSION")
        .args(["emit", "--kind", "ci.done"])
        .assert()
        .code(3);
}

#[test]
fn emit_missing_session_exits_5() {
    let root = TempDir::new().unwrap();
    tender(&root)
        .args(["emit", "--kind", "ci.done", "--session", "nope"])
        .assert()
        .code(5);
}

#[test]
fn emit_non_object_data_exits_2() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    tender(&root)
        .args([
            "emit",
            "--kind",
            "ci.done",
            "--session",
            "s1",
            "--data",
            "[1,2]",
        ])
        .assert()
        .code(2);
    tender(&root)
        .args([
            "emit",
            "--kind",
            "ci.done",
            "--session",
            "s1",
            "--data",
            "not json",
        ])
        .assert()
        .code(2);
}

#[test]
fn emit_invalid_parent_exits_2() {
    let root = TempDir::new().unwrap();
    finished_session(&root, "s1");

    tender(&root)
        .args([
            "emit",
            "--kind",
            "ci.done",
            "--session",
            "s1",
            "--parent",
            "not-a-uuid",
        ])
        .assert()
        .code(2);
}

// --- --best-effort: all failures exit 0 (hooks must never fail their host tool) ---

#[test]
fn emit_best_effort_swallows_all_failures() {
    let root = TempDir::new().unwrap();

    tender(&root)
        .args([
            "emit",
            "--kind",
            "run.reserved",
            "--session",
            "s1",
            "--best-effort",
        ])
        .assert()
        .success();
    tender(&root)
        .args([
            "emit",
            "--kind",
            "ci.done",
            "--session",
            "missing",
            "--best-effort",
        ])
        .assert()
        .success();
    tender(&root)
        .env_remove("TENDER_SESSION")
        .args(["emit", "--kind", "ci.done", "--best-effort"])
        .assert()
        .success();
}

// --- Orphan emitters → lost+found (spec §7, plan acceptance criterion 5) ---

#[test]
fn emit_from_pruned_session_lands_in_lost_found() {
    let root = TempDir::new().unwrap();
    let run_id = uuid::Uuid::now_v7().to_string();

    // Env context names a session whose dir no longer exists (pruned mid-run).
    tender(&root)
        .env("TENDER_SESSION", "gone")
        .env("TENDER_NAMESPACE", "default")
        .env("TENDER_RUN_ID", &run_id)
        .env("TENDER_GENERATION", "2")
        .args(["emit", "--kind", "hook.post_tool_use", "--best-effort"])
        .assert()
        .success();

    let lf = root.path().join(".tender/lost+found/events.jsonl");
    let content = std::fs::read_to_string(&lf).expect("lost+found log exists");
    let event: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    assert_eq!(event["kind"], "hook.post_tool_use");
    assert_eq!(event["session"], "gone");
    assert_eq!(event["namespace"], "default");
    assert_eq!(event["run_id"].as_str().unwrap(), run_id);
    assert_eq!(event["gen"], 2);
}
