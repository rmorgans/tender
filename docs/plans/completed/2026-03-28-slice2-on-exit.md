# Slice 2 — On-Exit Callbacks Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Execute `--on-exit` callback commands after the sidecar writes terminal state, so consumers like cmux get immediate exit signals without polling.

**Architecture:** Parse `--on-exit` (repeatable) in CLI, persist in LaunchSpec.on_exit. After write_meta_atomic in sidecar run_inner(), execute each callback as a best-effort child process with environment variables. Failures become warnings in meta.json.

**Tech Stack:** Rust, std::process::Command, clap.

**Security:** Callbacks are exec'd as direct argv (split on shell words), not passed through `sh -c`. Avoids shell injection.

**Quality gates:** `cargo clippy --all-targets` must pass. `cargo fmt` before each commit.

---

## Per-Slice Invariant Table

| Invariant | Why it matters | Enforced by | Tested by |
|-----------|---------------|-------------|-----------|
| Terminal state is durable before callbacks fire | Consumers can query meta.json during callback | Callback execution after write_meta_atomic | callback_runs_after_normal_exit |
| Callback failures only add warnings | Callback crash must not change exit reason | Warnings append, no state mutation | callback_failure_adds_warning |
| Callbacks see env vars | cmux needs session/namespace/reason | Command::envs() with TENDER_* vars | callback_sees_env_vars |
| Callbacks are exec'd, not shell-interpreted | Security boundary | Command::new(argv[0]).args(argv[1..]) | callback_with_special_chars |

---

## Batch 1: CLI flag + sidecar execution

**Task 1: Add --on-exit flag to Start command**

In src/main.rs Start variant, add:
```rust
#[arg(long = "on-exit", value_name = "COMMAND")]
on_exit: Vec<String>,
```

Pass through dispatch to cmd_start. In cmd_start, set:
```rust
launch_spec.on_exit = on_exit.to_vec();
```

**Task 2: Execute callbacks in sidecar after terminal state**

In src/sidecar.rs run_inner(), after write_meta_atomic (line 302) and before Ok(()):

```rust
// Execute on_exit callbacks — terminal state is already durable
for (i, callback_cmd) in meta.launch_spec().on_exit.iter().enumerate() {
    let argv = shell_words::split(callback_cmd)
        .unwrap_or_else(|_| vec![callback_cmd.clone()]);
    if argv.is_empty() {
        continue;
    }
    let result = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env("TENDER_SESSION", meta.session())
        .env("TENDER_NAMESPACE", meta.launch_spec().namespace.as_deref().unwrap_or("default"))
        .env("TENDER_RUN_ID", meta.run_id().to_string())
        .env("TENDER_GENERATION", meta.generation().to_string())
        .env("TENDER_EXIT_REASON", format!("{:?}", exit_reason))
        .env("TENDER_SESSION_DIR", session_dir.to_str().unwrap_or(""))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            meta.add_warning(format!("on_exit[{i}] failed (exit {}): {}", output.status, stderr.trim()));
        }
        Err(e) => {
            meta.add_warning(format!("on_exit[{i}] spawn failed: {e}"));
        }
        Ok(_) => {}
    }
}
// Re-write meta with any callback warnings
if !meta.launch_spec().on_exit.is_empty() {
    let _ = session::write_meta_atomic(&session, &meta);
}
```

NOTE: Need shell_words crate for safe shell-word splitting. Add `shell-words = "1"` to Cargo.toml. This avoids sh -c while still supporting quoted arguments like `--on-exit 'cmux notify "done"'`.

If you want to avoid the dependency, split on whitespace only. But shell_words is tiny and handles quoting correctly.

Actually — reconsider. The spec says "exec'd as direct argv, not passed through sh -c." But the user passes `--on-exit 'cmux notify "done"'` as a single string. We need SOME parsing. shell_words is the safe choice. Alternative: require the user to pass multiple --on-exit args for each word, which is bad UX.

Decision: use shell_words. It's 0 dependencies, ~200 lines, ISC licensed.

**Task 3: Write meta warnings after callbacks**

Already handled in Task 2 — re-write meta.json if any on_exit callbacks were present (to capture warnings).

## Batch 2: Integration tests

**Task 4: Test callback runs after normal exit**

```rust
#[test]
fn on_exit_callback_runs_after_normal_exit() {
    // Start with --on-exit that writes a marker file
    // Wait for terminal
    // Verify marker file exists
}
```

**Task 5: Test callback runs after forced kill**

**Task 6: Test callback sees TENDER_* env vars**

**Task 7: Test callback failure only adds warnings**

**Task 8: Full suite + quality gates**
