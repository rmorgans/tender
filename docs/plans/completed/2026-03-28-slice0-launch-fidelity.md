# Slice 0 — Launch Fidelity Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire `cwd` and `env` through the full stack so `tender start` faithfully launches child processes in the intended working directory with the intended environment.

**Architecture:** Extend `Platform::spawn_child` trait to accept `cwd: Option<&Path>` and `env: &BTreeMap<String, String>`. Add `--cwd` and `--env` CLI flags. Apply them in Unix impl. Update Windows skeleton signature. Existing `LaunchSpec` already has these fields and `canonical_hash()` already includes them.

**Trait signature note:** Adding `cwd` and `env` as individual params is a slice-local tradeoff. Passing `&LaunchSpec` directly is the likely later cleanup once more launch fields (namespace, on_exit, after) become runtime-active. Not worth the coupling for Slice 0.

**Tech Stack:** Rust, clap (CLI), std::process::Command, BTreeMap for sorted env.

**Conventions:** All new tests use `SERIAL.lock()` guard and `TempDir` from existing test patterns. Tests that start `sleep 60` must kill the session before returning.

**Quality gates:** `cargo clippy --all-targets` must pass (mandatory). `cargo clippy -- -W clippy::pedantic` is advisory — run it, fix what's reasonable in changed files, don't chase pre-existing nits. `cargo fmt` before each commit.

---

## Per-Slice Invariant Table

| Invariant | Why it matters | Enforced by | Tested by | Known exceptions |
|-----------|---------------|-------------|-----------|-----------------|
| `cwd` in LaunchSpec is applied during spawn | Child must run in requested directory | `cmd.current_dir()` in Unix spawn_child | `start_with_cwd_child_runs_in_requested_directory` | None |
| `env` in LaunchSpec overlays inherited env | Child must see overrides AND inherited PATH etc | `cmd.envs()` (additive, not replace) | `start_with_env_preserves_inherited_environment` | None |
| `cwd` and `env` participate in spec hash | Different launch config = different run identity | `canonical_hash()` via serde (already works — BTreeMap is sorted) | `start_with_different_cwd_is_spec_conflict`, `start_with_different_env_is_spec_conflict` | None |
| `--env KEY=VALUE` parsing validates at boundary | Malformed input must fail early, not silently produce empty values | `split_once('=')` with context error | `start_with_invalid_env_format_fails` (Task 6) | None |
| Platform trait signature includes cwd/env | All backends must accept launch config even if unimplemented | Trait method signature | Compile-time (Windows stub must match) | Windows returns Unsupported |

---

### Task 1: Test that child sees requested cwd

**Files:**
- Test: `tests/cli_start_idempotent.rs` (add new test)

**Step 1: Write the failing test**

Add to `tests/cli_start_idempotent.rs`:

```rust
#[test]
fn start_with_cwd_child_runs_in_requested_directory() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();
    let work_dir = root.path().join("myworkdir");
    std::fs::create_dir_all(&work_dir).unwrap();

    let out = tender(&root)
        .args([
            "start", "cwd-test",
            "--cwd", work_dir.to_str().unwrap(),
            "--", "pwd",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "start failed: {}", String::from_utf8_lossy(&out.stderr));

    wait_terminal(&root, "cwd-test");

    let log_out = tender(&root)
        .args(["log", "cwd-test", "--raw"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        log.contains(work_dir.to_str().unwrap()),
        "child should run in {work_dir:?}, got log: {log}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent start_with_cwd_child_runs_in_requested_directory -- --nocapture`

Expected: FAIL — clap rejects unknown `--cwd` flag.

**Step 3: Commit failing test**

```bash
git add tests/cli_start_idempotent.rs
git commit -m "test: child sees requested cwd (fails — no --cwd flag yet)"
```

---

### Task 2: Test that child sees overridden env vars

**Files:**
- Test: `tests/cli_start_idempotent.rs` (add new test)

**Step 1: Write the failing test**

Add to `tests/cli_start_idempotent.rs`:

```rust
#[test]
fn start_with_env_child_sees_overridden_vars() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .args([
            "start", "env-test",
            "--env", "TENDER_TEST_VAR=hello_from_tender",
            "--", "sh", "-c", "echo $TENDER_TEST_VAR",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "start failed: {}", String::from_utf8_lossy(&out.stderr));

    wait_terminal(&root, "env-test");

    let log_out = tender(&root)
        .args(["log", "env-test", "--raw"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        log.contains("hello_from_tender"),
        "child should see TENDER_TEST_VAR, got log: {log}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent start_with_env_child_sees_overridden_vars -- --nocapture`

Expected: FAIL — clap rejects unknown `--env` flag.

**Step 3: Commit failing test**

```bash
git add tests/cli_start_idempotent.rs
git commit -m "test: child sees overridden env vars (fails — no --env flag yet)"
```

---

### Task 3: Test that spec hash changes with cwd and env

**Files:**
- Test: `tests/cli_start_idempotent.rs` (add two new tests)

**Step 1: Write the failing tests**

Add to `tests/cli_start_idempotent.rs`:

```rust
#[test]
fn start_with_different_cwd_is_spec_conflict() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();
    let dir_a = root.path().join("dir_a");
    let dir_b = root.path().join("dir_b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let out1 = tender(&root)
        .args([
            "start", "cwd-conflict",
            "--cwd", dir_a.to_str().unwrap(),
            "--", "sleep", "60",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());

    let out2 = tender(&root)
        .args([
            "start", "cwd-conflict",
            "--cwd", dir_b.to_str().unwrap(),
            "--", "sleep", "60",
        ])
        .output()
        .unwrap();
    assert!(!out2.status.success(), "different cwd should be a spec conflict");
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr.contains("session conflict"), "expected conflict error, got: {stderr}");

    // Cleanup: kill the running session
    tender(&root).args(["kill", "cwd-conflict", "--force"]).output().unwrap();
    wait_terminal(&root, "cwd-conflict");
}

#[test]
fn start_with_different_env_is_spec_conflict() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out1 = tender(&root)
        .args([
            "start", "env-conflict",
            "--env", "FOO=bar",
            "--", "sleep", "60",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());

    let out2 = tender(&root)
        .args([
            "start", "env-conflict",
            "--env", "FOO=baz",
            "--", "sleep", "60",
        ])
        .output()
        .unwrap();
    assert!(!out2.status.success(), "different env should be a spec conflict");
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr.contains("session conflict"), "expected conflict error, got: {stderr}");

    // Cleanup: kill the running session
    tender(&root).args(["kill", "env-conflict", "--force"]).output().unwrap();
    wait_terminal(&root, "env-conflict");
}
```

**Step 2: Run tests to verify they fail**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent start_with_different -- --nocapture`

Expected: FAIL — clap rejects `--cwd` / `--env`.

**Step 3: Commit failing tests**

```bash
git add tests/cli_start_idempotent.rs
git commit -m "test: cwd and env changes cause spec conflict (fails — no flags yet)"
```

---

### Task 4: Add --cwd and --env CLI flags

**Files:**
- Modify: `src/main.rs` (Start variant + dispatch)
- Modify: `src/commands/start.rs` (signature + LaunchSpec building)

**Step 1: Add flags to Start command in `src/main.rs`**

Add after the `timeout` field in the Start variant (around line 25):

```rust
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,
```

Add `use std::path::PathBuf;` at the top if not present.

Update the dispatch call (around line 93) to pass the new args. The match arm stays returning a `Result<()>`:

```rust
        Commands::Start {
            name,
            cmd,
            stdin,
            replace,
            timeout,
            cwd,
            env_vars,
        } => commands::cmd_start(&name, cmd, stdin, replace, timeout, cwd.as_deref(), &env_vars),
```

**Step 2: Update cmd_start signature and LaunchSpec building**

In `src/commands/start.rs`, change the function signature (line 6):

```rust
pub fn cmd_start(
    name: &str,
    cmd: Vec<String>,
    stdin: bool,
    replace: bool,
    timeout: Option<u64>,
    cwd: Option<&std::path::Path>,
    env_vars: &[String],
) -> anyhow::Result<()> {
```

After `launch_spec.timeout_s = timeout;` (around line 30), add:

```rust
    launch_spec.cwd = cwd.map(|p| p.to_path_buf());
    for entry in env_vars {
        let (key, value) = entry
            .split_once('=')
            .with_context(|| format!("invalid --env format: expected KEY=VALUE, got: {entry}"))?;
        launch_spec.env.insert(key.to_string(), value.to_string());
    }
```

Requires `use anyhow::Context;` at the top of `start.rs` (if not already present).

**Step 3: Run the tests — flags parse but child won't see cwd/env yet**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent -- --nocapture`

Expected:
- `start_with_cwd_child_runs_in_requested_directory` — FAILS (child runs in default cwd)
- `start_with_env_child_sees_overridden_vars` — FAILS (child doesn't see env var)
- `start_with_different_cwd_is_spec_conflict` — PASSES (hash includes cwd)
- `start_with_different_env_is_spec_conflict` — PASSES (hash includes env)
- All existing tests — PASS

**Step 4: Commit**

```bash
git add src/main.rs src/commands/start.rs
git commit -m "feat: add --cwd and --env flags to start command"
```

---

### Task 5: Extend Platform::spawn_child to accept cwd and env

**Files:**
- Modify: `src/platform/mod.rs` (trait signature)
- Modify: `src/platform/unix.rs` (implementation)
- Modify: `src/platform/windows.rs` (stub signature)
- Modify: `src/sidecar.rs` (call site)

**Step 1: Change the trait signature in `src/platform/mod.rs`**

Replace the `spawn_child` method (around line 91):

```rust
    fn spawn_child(
        argv: &[String],
        stdin_piped: bool,
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> io::Result<Self::SupervisedChild>;
```

Add imports at the top of `mod.rs` if not present:

```rust
use std::collections::BTreeMap;
use std::path::Path;
```

**Step 2: Apply cwd and env in Unix implementation**

In `src/platform/unix.rs`, update `spawn_child` (around lines 59-90). Add these lines after `.stderr(std::process::Stdio::piped());` and before the `unsafe { cmd.pre_exec` block:

```rust
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        if !env.is_empty() {
            cmd.envs(env);
        }
```

Update the function signature to match the trait:

```rust
    fn spawn_child(
        argv: &[String],
        stdin_piped: bool,
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> io::Result<SupervisedChild> {
```

Add imports if not present:

```rust
use std::collections::BTreeMap;
use std::path::Path;
```

Note: `cmd.envs(env)` **adds** to the inherited environment. It does not replace it. This is correct — "preserve inherited environment by default, then overlay overrides."

**Step 3: Update Windows stub signature**

In `src/platform/windows.rs`, update the stub (around lines 75-77):

```rust
    fn spawn_child(
        _argv: &[String],
        _stdin_piped: bool,
        _cwd: Option<&Path>,
        _env: &BTreeMap<String, String>,
    ) -> io::Result<SupervisedChild> {
        Err(unsupported("spawn_child"))
    }
```

Add imports if not present:

```rust
use std::collections::BTreeMap;
use std::path::Path;
```

**Step 4: Update the call site in `src/sidecar.rs`**

Replace the spawn_child call (around lines 193-195):

```rust
    let stdin_piped = meta.launch_spec().stdin_mode == StdinMode::Pipe;
    let mut child = match Current::spawn_child(
        meta.launch_spec().argv(),
        stdin_piped,
        meta.launch_spec().cwd.as_deref(),
        &meta.launch_spec().env,
    ) {
```

**Step 5: Run ALL tests**

Run: `cd ~/Documents/Projects/tender && cargo test --tests -- --nocapture`

Expected: ALL tests pass, including the new cwd/env tests from Tasks 1-3.

**Step 6: Commit**

```bash
git add src/platform/mod.rs src/platform/unix.rs src/platform/windows.rs src/sidecar.rs
git commit -m "feat: apply cwd and env during child spawn via Platform trait"
```

---

### Task 6: Test boundary validation — invalid --env format fails early

**Files:**
- Test: `tests/cli_start_idempotent.rs` (add new test)

**Step 1: Write the test**

Boundary validation: malformed `--env` input must fail at the CLI layer, not silently produce bad state.

```rust
#[test]
fn start_with_invalid_env_format_fails() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .args([
            "start", "bad-env",
            "--env", "NO_EQUALS_SIGN",
            "--", "true",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "malformed --env should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("KEY=VALUE"),
        "error should mention expected format, got: {stderr}"
    );
}
```

**Step 2: Run test**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent start_with_invalid_env_format_fails -- --nocapture`

Expected: PASS (boundary validation was added in Task 4).

**Step 3: Commit**

```bash
git add tests/cli_start_idempotent.rs
git commit -m "test: boundary validation — invalid --env format fails early"
```

---

### Task 7: Test that child inherits parent env plus overrides

**Files:**
- Test: `tests/cli_start_idempotent.rs` (add new test)

**Step 1: Write the test**

```rust
#[test]
fn start_with_env_preserves_inherited_environment() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();

    let out = tender(&root)
        .args([
            "start", "env-inherit",
            "--env", "TENDER_EXTRA=added",
            "--", "sh", "-c", "echo PATH=$PATH TENDER_EXTRA=$TENDER_EXTRA",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "start failed: {}", String::from_utf8_lossy(&out.stderr));

    wait_terminal(&root, "env-inherit");

    let log_out = tender(&root)
        .args(["log", "env-inherit", "--raw"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(log.contains("PATH="), "child should inherit PATH from parent");
    assert!(log.contains("TENDER_EXTRA=added"), "child should see override");
}
```

**Step 2: Run test**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent start_with_env_preserves_inherited_environment -- --nocapture`

Expected: PASS.

**Step 3: Commit**

```bash
git add tests/cli_start_idempotent.rs
git commit -m "test: verify env overrides preserve inherited environment"
```

---

### Task 8: Test idempotent start with cwd and env

**Files:**
- Test: `tests/cli_start_idempotent.rs` (add new test)

**Step 1: Write the test**

```rust
#[test]
fn start_with_same_cwd_and_env_is_idempotent() {
    let _guard = SERIAL.lock().unwrap();
    let root = TempDir::new().unwrap();
    let work_dir = root.path().join("samedir");
    std::fs::create_dir_all(&work_dir).unwrap();

    let out1 = tender(&root)
        .args([
            "start", "idem-cwd-env",
            "--cwd", work_dir.to_str().unwrap(),
            "--env", "FOO=bar",
            "--", "sleep", "60",
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let meta1: serde_json::Value = serde_json::from_slice(&out1.stdout).unwrap();
    let run_id1 = meta1["run_id"].as_str().unwrap().to_string();

    let out2 = tender(&root)
        .args([
            "start", "idem-cwd-env",
            "--cwd", work_dir.to_str().unwrap(),
            "--env", "FOO=bar",
            "--", "sleep", "60",
        ])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let meta2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    let run_id2 = meta2["run_id"].as_str().unwrap().to_string();

    assert_eq!(run_id1, run_id2, "same spec with cwd+env should be idempotent");

    // Cleanup: kill the running session
    tender(&root).args(["kill", "idem-cwd-env", "--force"]).output().unwrap();
    wait_terminal(&root, "idem-cwd-env");
}
```

**Step 2: Run test**

Run: `cd ~/Documents/Projects/tender && cargo test --test cli_start_idempotent start_with_same_cwd_and_env_is_idempotent -- --nocapture`

Expected: PASS.

**Step 3: Commit**

```bash
git add tests/cli_start_idempotent.rs
git commit -m "test: verify idempotent start with cwd and env"
```

---

### Task 9: Run full test suite and quality gates

**Step 1: Run all tests**

Run: `cd ~/Documents/Projects/tender && cargo test --tests`

Expected: All test suites pass. 0 failures. Total should be ~186 tests (178 existing + 8 new).

**Step 2: Run clippy (mandatory)**

Run: `cd ~/Documents/Projects/tender && cargo clippy --all-targets`

Expected: No warnings.

**Step 3: Run clippy pedantic (advisory)**

Run: `cd ~/Documents/Projects/tender && cargo clippy --all-targets -- -W clippy::pedantic`

Expected: Review any warnings in changed files. Fix what's reasonable. Don't chase pre-existing nits.

**Step 3: Format and commit if needed**

```bash
cd ~/Documents/Projects/tender && cargo fmt
git add -A
git commit -m "style: cargo fmt" # only if fmt changed anything
```

---

## Summary

| Task | What | New tests |
|------|------|-----------|
| 1 | Test: child sees cwd | 1 |
| 2 | Test: child sees env | 1 |
| 3 | Test: cwd/env change = spec conflict | 2 |
| 4 | Add --cwd and --env CLI flags + boundary validation | 0 (makes conflict tests pass) |
| 5 | Wire through Platform trait + sidecar | 0 (makes cwd/env tests pass) |
| 6 | Test: boundary validation — invalid --env fails | 1 |
| 7 | Test: env preserves inherited env | 1 |
| 8 | Test: idempotent with cwd+env | 1 |
| 9 | Full suite + clippy pedantic | 0 |

**Total new tests: 8**
**Files modified: 6** (main.rs, commands/start.rs, platform/mod.rs, platform/unix.rs, platform/windows.rs, sidecar.rs)
**Test file: 1** (tests/cli_start_idempotent.rs)
