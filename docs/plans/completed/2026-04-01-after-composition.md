---
id: after-composition
depends_on: []
links: []
---

# --after Dependency Chaining

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `--after <session>` flag so a session waits for dependencies to reach terminal state before spawning its child.

**Architecture:** At CLI bind time, `--after` resolves each dependency's `run_id` from its `meta.json` and stores bindings in `LaunchSpec.after` (struct already exists). The sidecar, before spawning the child, polls each dependency's `meta.json` until all are satisfied or a failure is hit. A new `DependencyFailed` terminal state with a machine-readable `reason` field handles all pre-spawn exits from the dependency wait phase.

**Tech Stack:** Rust, clap (CLI), serde (serialization), assert_cmd (integration tests)

---

## CLI

```bash
tender start job2 --after job1 -- cmd              # wait for job1 exit 0
tender start job2 --after job1 --any-exit -- cmd   # wait regardless of exit code
tender start job2 --after job1 --after job3 -- cmd # wait for both
```

## Behavior

1. `--after <session>` adds a dependency to the LaunchSpec
2. At bind time, the CLI captures the target session's current `run_id` from its `meta.json`
3. Stored as `{ session: "<name>", run_id: "<uuid>" }` in LaunchSpec
4. The sidecar, before spawning the child, polls each dependency's `meta.json` until:
   - Target reaches terminal with exit 0 → proceed
   - Target reaches terminal with non-zero exit → `DependencyFailed { reason: Failed }`
   - Target's `run_id` changes (replaced) → `DependencyFailed { reason: Failed }`
   - `--any-exit` flag: proceed on any terminal state regardless of exit code
5. Once all satisfied, sidecar spawns the child normally

## LaunchSpec Changes

```json
{
  "argv": ["cmd"],
  "after": [
    {"session": "job1", "run_id": "019..."},
    {"session": "job3", "run_id": "019..."}
  ],
  "after_any_exit": false
}
```

The `after` array is included in `canonical_hash` (via serde). Same command with different deps is a different spec (conflict on idempotent start).

`DependencyBinding` and `LaunchSpec.after` already exist. Only `after_any_exit: bool` needs adding.

## Sidecar Wait Loop

- Poll interval: 500ms
- Sidecar holds the session lock during the wait:
  - `tender status` shows `Starting`
  - `tender kill` can kill during the wait (see Kill During Wait below)
  - `--timeout` applies to total time including the wait
- If dependency session does not exist at sidecar start: fail immediately
- Ready signal fires **before** the wait loop — `tender start` returns immediately with Starting state

## DependencyFailed State

New terminal state with a machine-readable `reason` discriminator:

```rust
DependencyFailed {
    ended_at: EpochTimestamp,
    reason: DepFailReason,
}
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dep_reason")]
pub enum DepFailReason {
    /// Dependency exited non-zero, was not found, or was replaced.
    Failed,
    /// Timeout expired during dependency wait (before child spawn).
    TimedOut,
    /// User-initiated kill during dependency wait (before child spawn).
    Killed,
}
```

Serialized example:
```json
{"status": "DependencyFailed", "dep_reason": "Killed", "ended_at": 1712000000}
```

## Exit Code Semantics

| Condition | Exit Code | State |
|-----------|-----------|-------|
| All deps satisfied, child runs and exits 0 | 0 | Exited/ExitedOk |
| All deps satisfied, child runs and exits non-zero | child code | Exited/ExitedError |
| Dep exited non-zero (without --any-exit) | 4 | DependencyFailed/Failed |
| Dep was replaced (run_id mismatch) | 4 | DependencyFailed/Failed |
| Dep session does not exist | 4 | DependencyFailed/Failed |
| Timeout during dependency wait | 124 | DependencyFailed/TimedOut |
| Kill during dependency wait | 137 | DependencyFailed/Killed |

Exit codes 124 and 137 match the existing timeout and kill contracts. Exit code 4 is new for dependency-specific failures.

## Kill During Dependency Wait

Current `tender kill` (kill.rs:31-41) bails with `"no_child"` when status is Starting. This must change:

1. `tender kill job2` → session is Starting, no child, but sidecar is alive (locked)
2. Kill command writes `kill_request` (run_id-scoped, same as today)
3. Sidecar's wait loop checks for `kill_request` on each poll cycle
4. Sidecar consumes the request, validates run_id, transitions to `DependencyFailed { reason: Killed }`
5. Kill command's existing wait-for-terminal loop picks up the result

The kill command change: when Starting + sidecar alive (locked), write kill_request instead of returning `no_child`. This is useful independent of `--after` (sidecar could be slow to spawn).

## Timeout During Dependency Wait

Timeout is handled entirely by the wait loop (not the timeout thread, which hasn't started yet). If `timeout_s` is set, the wait loop computes a deadline at entry and checks it each iteration. On expiry → `DependencyFailed { reason: TimedOut }`.

## Idempotent Start on Starting State

Current `try_idempotent_start` (start.rs:218) errors on Starting state. With `--after`, sessions legitimately sit in Starting while waiting. Fix:

- Starting + locked (sidecar alive) + same spec hash → idempotent return (re-read and return existing meta)
- Starting + locked + different spec hash → conflict error
- Starting + unlocked → orphan (existing cleanup path)

## Namespace Resolution

`--after job1` resolves in the same namespace as the new session. Cross-namespace is out of scope.

---

## Implementation Tasks

### Task 1: Add `after_any_exit` to LaunchSpec + `DepFailReason` enum

**Files:**
- Modify: `src/model/spec.rs` — add `after_any_exit: bool` field
- Create: `src/model/dep_fail.rs` — `DepFailReason` enum
- Modify: `src/model/mod.rs` — add `pub mod dep_fail;`

**Step 1: Add `after_any_exit` field**

In `src/model/spec.rs`, add after the `after` field:

```rust
#[serde(skip_serializing_if = "is_false", default)]
pub after_any_exit: bool,
```

Add helper at module level:

```rust
fn is_false(b: &bool) -> bool {
    !b
}
```

Add to `LaunchSpec::new()`: `after_any_exit: false,`

Add to `Raw` struct in `Deserialize` impl:

```rust
#[serde(default)]
after_any_exit: bool,
```

And in the constructed `Ok(Self { ... })`: `after_any_exit: raw.after_any_exit,`

**Step 2: Create `DepFailReason` enum**

Create `src/model/dep_fail.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Why a session failed during the dependency-wait phase.
/// Machine-readable discriminator inside `DependencyFailed` state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dep_reason")]
pub enum DepFailReason {
    /// Dependency exited non-zero, was not found, or was replaced.
    Failed,
    /// Timeout expired during dependency wait (before child spawn).
    TimedOut,
    /// User-initiated kill during dependency wait (before child spawn).
    Killed,
}
```

Add `pub mod dep_fail;` to `src/model/mod.rs`.

**Step 3: Verify**

Run: `cargo test`
Expected: All pass. New field defaults to false, new enum is unused.

**Step 4: Commit**

```
feat(model): add after_any_exit field and DepFailReason enum
```

---

### Task 2: Add `DependencyFailed` terminal state

**Files:**
- Modify: `src/model/state.rs` — add variant
- Modify: `src/model/transition.rs` — add transition + status_name
- Modify: `tests/model_transitions.rs` — add tests

**Step 1: Write failing test**

Add to `tests/model_transitions.rs`:

```rust
#[test]
fn starting_to_dependency_failed() {
    let mut meta = make_meta();
    assert!(!meta.status().is_terminal());
    meta.transition_dependency_failed(
        EpochTimestamp::now(),
        tender::model::dep_fail::DepFailReason::Failed,
    )
    .unwrap();
    assert!(meta.status().is_terminal());
    assert!(matches!(
        meta.status(),
        RunStatus::DependencyFailed { .. }
    ));
}

#[test]
fn running_to_dependency_failed_is_illegal() {
    let mut meta = make_meta();
    meta.transition_running(make_child()).unwrap();
    let result = meta.transition_dependency_failed(
        EpochTimestamp::now(),
        tender::model::dep_fail::DepFailReason::Failed,
    );
    assert!(result.is_err());
}
```

Run: `cargo test model_transitions`
Expected: FAIL — variant and transition don't exist.

**Step 2: Add `DependencyFailed` to `RunStatus`**

In `src/model/state.rs`, add import and variant:

```rust
use super::dep_fail::DepFailReason;
```

After `SidecarLost`:

```rust
/// Dependency wait phase failed before child was spawned.
DependencyFailed {
    ended_at: EpochTimestamp,
    #[serde(flatten)]
    reason: DepFailReason,
},
```

Update `child()` — add arm: `RunStatus::DependencyFailed { .. } => None,`

Update `ended_at()` — add arm in the terminal match: `| RunStatus::DependencyFailed { ended_at, .. }`

(`is_terminal` needs no change — it checks for non-terminal variants.)

**Step 3: Add transition + status_name**

In `src/model/transition.rs`, add to `status_name`:

```rust
RunStatus::DependencyFailed { .. } => "DependencyFailed",
```

Add transition method:

```rust
/// Transition Starting → DependencyFailed.
pub fn transition_dependency_failed(
    &mut self,
    ended_at: EpochTimestamp,
    reason: crate::model::dep_fail::DepFailReason,
) -> Result<(), TransitionError> {
    match self.status() {
        RunStatus::Starting => {
            *self.status_mut() = RunStatus::DependencyFailed { ended_at, reason };
            Ok(())
        }
        RunStatus::Running { .. } => Err(TransitionError::Illegal {
            from: "Running",
            to: "DependencyFailed",
        }),
        _ => Err(TransitionError::AlreadyTerminal {
            from: status_name(self.status()),
        }),
    }
}
```

**Step 4: Wire exit codes in wait and run commands**

In `src/commands/wait.rs`, add after `SidecarLost` arm:

```rust
RunStatus::DependencyFailed { reason, .. } => {
    use tender::model::dep_fail::DepFailReason;
    match reason {
        DepFailReason::Failed => std::process::exit(4),
        DepFailReason::TimedOut => std::process::exit(124),
        DepFailReason::Killed => std::process::exit(137),
    }
}
```

In `src/commands/run.rs` `foreground_wait()`, same pattern after `SidecarLost`:

```rust
RunStatus::DependencyFailed { reason, .. } => {
    use tender::model::dep_fail::DepFailReason;
    match reason {
        DepFailReason::Failed => std::process::exit(4),
        DepFailReason::TimedOut => std::process::exit(124),
        DepFailReason::Killed => std::process::exit(137),
    }
}
```

**Step 5: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 6: Commit**

```
feat(state): add DependencyFailed terminal state with reason discriminator
```

---

### Task 3: Add `--after` and `--any-exit` CLI flags + bind-time resolution

**Files:**
- Modify: `src/main.rs` — add CLI args
- Modify: `src/commands/start.rs` — resolve deps at bind time

**Step 1: Write CLI parsing test**

Add `tests/cli_after.rs`:

```rust
mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

/// --after resolves dependency run_id at bind time.
/// Verifies the launch_spec in meta.json contains the binding.
#[test]
fn after_bind_captures_run_id() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (short-lived)
    harness::tender(&root)
        .args(["start", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    // Read job1's run_id
    let job1_meta_path = root
        .path()
        .join(".tender/sessions/default/job1/meta.json");
    let job1_meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&job1_meta_path).unwrap()).unwrap();
    let job1_run_id = job1_meta["run_id"].as_str().unwrap();

    // Start job2 --after job1
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job2");

    // Verify job2's launch_spec.after contains job1's run_id
    let job2_meta_path = root
        .path()
        .join(".tender/sessions/default/job2/meta.json");
    let job2_meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&job2_meta_path).unwrap()).unwrap();
    let after = &job2_meta["launch_spec"]["after"];
    assert_eq!(after[0]["session"].as_str(), Some("job1"));
    assert_eq!(after[0]["run_id"].as_str(), Some(job1_run_id));
}

/// --after nonexistent session fails at bind time.
#[test]
fn after_nonexistent_session_fails_at_bind() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job2", "--after", "nonexistent", "--", "true"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session not found"));
}
```

Run: `cargo test cli_after`
Expected: FAIL — `--after` flag doesn't exist.

**Step 2: Add CLI flags**

In `src/main.rs`, add to `Commands::Start` (before `cmd`):

```rust
/// Wait for session(s) to exit before starting (repeatable)
#[arg(long = "after", value_name = "SESSION")]
after: Vec<String>,
/// Proceed even if dependency exits non-zero
#[arg(long = "any-exit")]
any_exit: bool,
```

Add same to `Commands::Run` (before `args`):

```rust
/// Wait for session(s) to exit before starting (repeatable)
#[arg(long = "after", value_name = "SESSION")]
after: Vec<String>,
/// Proceed even if dependency exits non-zero
#[arg(long = "any-exit")]
any_exit: bool,
```

Pass through in `Commands::Start` match arm and `Commands::Run` match arm.

**Step 3: Update `cmd_start` and `launch_session` signatures**

Add `after: &[String]` and `any_exit: bool` parameters. In `launch_session`, after building the spec and before session creation:

```rust
// Resolve --after: capture each dependency's run_id at bind time
if !after.is_empty() {
    for dep_name in after {
        let dep_session_name = SessionName::new(dep_name)?;
        let dep_session = session::open(&root, namespace, &dep_session_name)?
            .ok_or_else(|| anyhow::anyhow!("--after: session not found: {dep_name}"))?;
        let dep_meta = session::read_meta(&dep_session)?;
        launch_spec.after.push(tender::model::spec::DependencyBinding {
            session: dep_session_name,
            run_id: dep_meta.run_id(),
        });
    }
    launch_spec.after_any_exit = any_exit;
}
```

**Step 4: Update `cmd_run` to pass through**

Add `after` and `any_exit` to `cmd_run` signature and to `EffectiveOptions`. Pass them through to `launch_session`. CLI overrides — no directive equivalent for `--after` in this plan.

**Step 5: Run tests**

Run: `cargo test cli_after`
Expected: PASS — bind-time tests verify run_id capture and not-found error.

**Step 6: Commit**

```
feat(cli): add --after and --any-exit flags with bind-time resolution
```

---

### Task 4: Fix idempotent start for Starting state

**Files:**
- Modify: `src/commands/start.rs` — update `try_idempotent_start`

**Step 1: Write failing test**

Add to `tests/cli_after.rs`:

```rust
/// Idempotent start on Starting session (waiting for deps): same spec → return existing.
#[test]
fn after_idempotent_on_starting() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (long-running so job2 stays in Starting)
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1 (enters Starting, waits)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "echo", "done"])
        .assert()
        .success();

    // Give sidecar time to enter wait loop
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Second start with identical args: should succeed (idempotent)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "echo", "done"])
        .assert()
        .success();

    // Clean up
    let _ = harness::tender(&root).args(["kill", "job1", "--force"]).assert();
    let _ = harness::tender(&root).args(["kill", "job2"]).assert();
}
```

Run: `cargo test cli_after::after_idempotent_on_starting`
Expected: FAIL — current code bails on Starting state.

**Step 2: Update `try_idempotent_start`**

Replace the `else` branch (starting state, line 218-220) in `try_idempotent_start`:

```rust
} else if matches!(existing_meta.status(), tender::model::state::RunStatus::Starting) {
    // Starting state: sidecar is alive (could be waiting on --after deps)
    if session::is_locked(&existing).unwrap_or(false) {
        // Sidecar alive — check spec match for idempotent return
        if existing_meta.launch_spec_hash() == launch_spec.canonical_hash() {
            Ok(None) // Idempotent
        } else {
            anyhow::bail!(
                "session conflict: {name} is starting with a different launch spec (use --replace to override)"
            );
        }
    } else {
        // Starting + unlocked = orphan
        super::status::cleanup_orphan_dir(&session_path);
        let session = session::create(root, namespace, session_name)?;
        Ok(Some(session))
    }
} else {
```

(The final `else` is now unreachable since we cover Running, terminal, and Starting, but keep it as a defensive bail.)

**Step 3: Run tests**

Run: `cargo test cli_after`
Expected: PASS.

**Step 4: Commit**

```
fix(start): handle idempotent start on Starting sessions
```

---

### Task 5: Implement sidecar dependency wait loop

**Files:**
- Modify: `src/sidecar.rs`

This is the core logic. The sidecar, after acquiring the lock and reading the spec but before spawning the child, polls dependencies.

**Step 1: Write integration test — waits for running dependency**

Add to `tests/cli_after.rs`:

```rust
/// job2 waits while job1 is still running, then runs after job1 exits.
#[test]
fn after_waits_for_running_dependency() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (runs for 2s)
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "2"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();

    // job2 should be Starting (waiting)
    let meta_path = root
        .path()
        .join(".tender/sessions/default/job2/meta.json");
    let content = std::fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(meta["status"].as_str(), Some("Starting"));

    // Wait for both to finish
    harness::wait_terminal(&root, "job1");
    let meta2 = harness::wait_terminal(&root, "job2");
    assert_eq!(meta2["status"].as_str(), Some("Exited"));
    assert_eq!(meta2["reason"].as_str(), Some("ExitedOk"));
}
```

Run: `cargo test cli_after::after_waits_for_running_dependency`
Expected: FAIL — sidecar ignores `after`, spawns child immediately.

**Step 2: Implement `wait_for_dependencies` function**

Add to `src/sidecar.rs`:

```rust
use crate::model::dep_fail::DepFailReason;

/// Outcome of the dependency wait phase.
enum DepWaitOutcome {
    /// All dependencies satisfied — proceed to spawn.
    Satisfied,
    /// A dependency failed (non-zero exit, not found, replaced).
    Failed(String),
    /// Timeout expired during the wait.
    TimedOut(String),
    /// Kill request received during the wait.
    Killed(String),
}

/// Poll dependency meta.json files until all reach terminal state.
fn wait_for_dependencies(
    session_root: &SessionRoot,
    namespace: &Namespace,
    spec: &LaunchSpec,
    timeout_s: Option<u64>,
    session_dir: &Path,
    run_id: &RunId,
) -> DepWaitOutcome {
    let deadline = timeout_s
        .map(|t| std::time::Instant::now() + std::time::Duration::from_secs(t));
    let kill_request_path = session_dir.join("kill_request");

    loop {
        // Check kill request
        if kill_request_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&kill_request_path) {
                if let Ok(req) = serde_json::from_str::<serde_json::Value>(&content) {
                    if req["run_id"].as_str() == Some(&run_id.to_string()) {
                        let _ = std::fs::remove_file(&kill_request_path);
                        return DepWaitOutcome::Killed(
                            "killed during dependency wait".into(),
                        );
                    }
                }
            }
            // Wrong run_id or malformed — ignore
            let _ = std::fs::remove_file(&kill_request_path);
        }

        // Check timeout
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                return DepWaitOutcome::TimedOut(
                    "timeout expired during dependency wait".into(),
                );
            }
        }

        // Poll all dependencies
        let mut all_satisfied = true;
        for dep in &spec.after {
            let dep_session = match session::open(session_root, namespace, &dep.session) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return DepWaitOutcome::Failed(format!(
                        "dependency session not found: {}",
                        dep.session
                    ));
                }
                Err(e) => {
                    return DepWaitOutcome::Failed(format!(
                        "failed to open dependency {}: {e}",
                        dep.session
                    ));
                }
            };

            let dep_meta = match session::read_meta(&dep_session) {
                Ok(m) => m,
                Err(e) => {
                    return DepWaitOutcome::Failed(format!(
                        "failed to read dependency {}: {e}",
                        dep.session
                    ));
                }
            };

            // Check run_id — reject if dependency was replaced
            if dep_meta.run_id() != dep.run_id {
                return DepWaitOutcome::Failed(format!(
                    "dependency {} was replaced (bound run_id {}, found {})",
                    dep.session, dep.run_id, dep_meta.run_id()
                ));
            }

            if dep_meta.status().is_terminal() {
                if !spec.after_any_exit {
                    use crate::model::state::{ExitReason, RunStatus};
                    match dep_meta.status() {
                        RunStatus::Exited {
                            how: ExitReason::ExitedOk,
                            ..
                        } => {} // satisfied
                        _ => {
                            return DepWaitOutcome::Failed(format!(
                                "dependency {} exited with non-success state",
                                dep.session
                            ));
                        }
                    }
                }
                // satisfied (or --any-exit)
            } else {
                all_satisfied = false;
            }
        }

        if all_satisfied {
            return DepWaitOutcome::Satisfied;
        }

        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
```

**Step 3: Wire into `run_inner`**

In `run_inner`, after the ready-fd seal block (~line 258) and before the child spawn (~line 276), insert:

```rust
// --- Wait for --after dependencies ---
let has_deps = !meta.launch_spec().after.is_empty();
if has_deps {
    // Signal readiness BEFORE waiting — CLI unblocks, status shows Starting.
    session::write_meta_atomic(&session, &meta)?;
    signal_meta_snapshot(ready, &meta)?;

    match wait_for_dependencies(
        &session_root,
        &namespace,
        meta.launch_spec(),
        meta.launch_spec().timeout_s,
        session_dir,
        &run_id,
    ) {
        DepWaitOutcome::Satisfied => {} // proceed to spawn
        DepWaitOutcome::Failed(msg) => {
            meta.add_warning(msg);
            meta.transition_dependency_failed(EpochTimestamp::now(), DepFailReason::Failed)?;
            session::write_meta_atomic(&session, &meta)?;
            return Ok(());
        }
        DepWaitOutcome::TimedOut(msg) => {
            meta.add_warning(msg);
            meta.transition_dependency_failed(EpochTimestamp::now(), DepFailReason::TimedOut)?;
            session::write_meta_atomic(&session, &meta)?;
            return Ok(());
        }
        DepWaitOutcome::Killed(msg) => {
            meta.add_warning(msg);
            meta.transition_dependency_failed(EpochTimestamp::now(), DepFailReason::Killed)?;
            session::write_meta_atomic(&session, &meta)?;
            return Ok(());
        }
    }
}
```

Then adjust the spawn-success and spawn-failure paths to only signal ready when `!has_deps`:

The SpawnFailed path (~line 287): wrap `signal_meta_snapshot(ready, &meta)?;` in `if !has_deps { ... }`

The child-identity-failure path (~line 305): same wrap.

The Running transition path (~line 328): wrap `signal_meta_snapshot(ready, &meta)?;` in `if !has_deps { ... }`

**Step 4: Run tests**

Run: `cargo test cli_after`
Expected: All pass including the new `after_waits_for_running_dependency` test.

**Step 5: Commit**

```
feat(sidecar): implement dependency wait loop for --after
```

---

### Task 6: Fix `tender kill` for Starting state

**Files:**
- Modify: `src/commands/kill.rs`

**Step 1: Write test**

Add to `tests/cli_after.rs`:

```rust
/// Kill during dependency wait → DependencyFailed/Killed.
#[test]
fn kill_during_dependency_wait() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    // Start job1 (long-running)
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "60"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1 (enters wait loop)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();

    // Give sidecar time to enter wait loop
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Kill job2
    harness::tender(&root)
        .args(["kill", "job2"])
        .assert()
        .success();

    let meta = harness::wait_terminal(&root, "job2");
    assert_eq!(meta["status"].as_str(), Some("DependencyFailed"));
    assert_eq!(meta["dep_reason"].as_str(), Some("Killed"));

    // Clean up job1
    let _ = harness::tender(&root).args(["kill", "job1", "--force"]).assert();
}
```

Run: `cargo test cli_after::kill_during_dependency_wait`
Expected: FAIL — kill command returns "no_child" and doesn't write kill_request.

**Step 2: Update kill command**

In `src/commands/kill.rs`, replace the `None` arm (lines 33-41):

```rust
None => {
    // Starting state with no child — sidecar may be in dependency wait.
    // If sidecar is alive, signal it via kill_request (same as Running path).
    let sidecar_alive = session::is_locked(&session).unwrap_or(false);
    if sidecar_alive {
        let run_id = meta.run_id().to_string();
        let request = serde_json::json!({ "force": force, "run_id": run_id });
        let kill_request_path = session.path().join("kill_request");
        let kill_request_tmp = session.path().join("kill_request.tmp");
        std::fs::write(&kill_request_tmp, request.to_string())?;
        std::fs::rename(&kill_request_tmp, &kill_request_path)?;

        // Wait for sidecar to write terminal state
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(m) = session::read_meta(&session) {
                if m.status().is_terminal() {
                    let json = serde_json::to_string_pretty(&m)?;
                    println!("{json}");
                    return Ok(());
                }
            }
        }
        // Sidecar didn't act — fall through to report
    }
    println!(
        "{}",
        serde_json::json!({"session": name, "result": "no_child"})
    );
    return Ok(());
}
```

**Step 3: Run tests**

Run: `cargo test cli_after`
Expected: All pass.

**Step 4: Commit**

```
fix(kill): write kill_request for Starting sessions with live sidecar
```

---

### Task 7: Integration tests for remaining failure cases

**Files:**
- Modify: `tests/cli_after.rs`

**Step 1: Dependency exits non-zero**

```rust
#[test]
fn after_dependency_exits_nonzero() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "false"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();

    let meta = harness::wait_terminal(&root, "job2");
    assert_eq!(meta["status"].as_str(), Some("DependencyFailed"));
    assert_eq!(meta["dep_reason"].as_str(), Some("Failed"));
}
```

**Step 2: --any-exit proceeds on non-zero**

```rust
#[test]
fn after_any_exit_proceeds_on_failure() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "false"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--any-exit", "--", "true"])
        .assert()
        .success();

    let meta = harness::wait_terminal(&root, "job2");
    assert_eq!(meta["status"].as_str(), Some("Exited"));
    assert_eq!(meta["reason"].as_str(), Some("ExitedOk"));
}
```

**Step 3: run_id mismatch (dependency replaced)**

```rust
#[test]
fn after_run_id_mismatch_fails() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    // Start job2 --after job1 (captures run_id)
    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();

    // Replace job1 (new run_id)
    harness::tender(&root)
        .args(["start", "job1", "--replace", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    // job2 should fail (run_id mismatch)
    let meta = harness::wait_terminal(&root, "job2");
    assert_eq!(meta["status"].as_str(), Some("DependencyFailed"));
    assert_eq!(meta["dep_reason"].as_str(), Some("Failed"));
}
```

**Step 4: Timeout during dependency wait**

```rust
#[test]
fn after_timeout_during_wait() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "60"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--timeout", "2", "--", "true"])
        .assert()
        .success();

    let meta = harness::wait_terminal(&root, "job2");
    assert_eq!(meta["status"].as_str(), Some("DependencyFailed"));
    assert_eq!(meta["dep_reason"].as_str(), Some("TimedOut"));

    let _ = harness::tender(&root).args(["kill", "job1", "--force"]).assert();
}
```

**Step 5: Multiple --after dependencies**

```rust
#[test]
fn after_multiple_dependencies() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "true"])
        .assert()
        .success();
    harness::tender(&root)
        .args(["start", "job3", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");
    harness::wait_terminal(&root, "job3");

    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--after", "job3", "--", "true"])
        .assert()
        .success();

    let meta = harness::wait_terminal(&root, "job2");
    assert_eq!(meta["status"].as_str(), Some("Exited"));
    assert_eq!(meta["reason"].as_str(), Some("ExitedOk"));
}
```

**Step 6: Idempotent start with different deps → conflict**

```rust
#[test]
fn after_idempotent_different_deps_conflicts() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::tender(&root)
        .args(["start", "job3", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");
    harness::wait_running(&root, "job3");

    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "sleep", "30"])
        .assert()
        .success();

    std::thread::sleep(std::time::Duration::from_millis(500));

    // Different dep → conflict
    harness::tender(&root)
        .args(["start", "job2", "--after", "job3", "--", "sleep", "30"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session conflict"));

    let _ = harness::tender(&root).args(["kill", "job1", "--force"]).assert();
    let _ = harness::tender(&root).args(["kill", "job2"]).assert();
    let _ = harness::tender(&root).args(["kill", "job3", "--force"]).assert();
}
```

**Step 7: `tender wait` exit code for DependencyFailed**

```rust
#[test]
fn wait_dependency_failed_exits_4() {
    let _lock = SERIAL.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "job1", "--", "false"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    harness::tender(&root)
        .args(["start", "job2", "--after", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job2");

    harness::tender(&root)
        .args(["wait", "job2"])
        .assert()
        .code(4);
}
```

**Step 8: Run all tests**

Run: `cargo test`
Expected: All pass.

**Step 9: Commit**

```
test: comprehensive integration tests for --after dependency chaining
```

---

### Task 8: Final verification

**Step 1: Full test suite**

Run: `cargo test`
Expected: All pass.

**Step 2: Clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

**Step 3: Commit**

```
feat: implement --after dependency chaining (after-composition)
```

---

## Testing Matrix

| Test | Exercises |
|------|-----------|
| `after_bind_captures_run_id` | CLI parsing, bind-time resolution |
| `after_nonexistent_session_fails_at_bind` | Bind-time validation |
| `after_idempotent_on_starting` | Idempotent start on Starting state |
| `after_waits_for_running_dependency` | Core wait loop, ready signal timing |
| `kill_during_dependency_wait` | Kill command + sidecar kill_request in wait |
| `after_dependency_exits_nonzero` | DependencyFailed/Failed |
| `after_any_exit_proceeds_on_failure` | --any-exit flag |
| `after_run_id_mismatch_fails` | run_id mismatch detection |
| `after_timeout_during_wait` | Timeout in wait phase |
| `after_multiple_dependencies` | Multiple --after flags |
| `after_idempotent_different_deps_conflicts` | Spec hash mismatch |
| `wait_dependency_failed_exits_4` | Exit code propagation |

## Design Decisions

**DependencyFailed with reason discriminator**: The child lifecycle states (`Exited { how: Killed/TimedOut }`) require a `ProcessIdentity`. During the dependency wait, no child exists. Rather than making `child` optional in `Exited` (ripple through codebase) or adding standalone `Killed`/`TimedOut` states, `DependencyFailed` carries a `DepFailReason` that preserves the exit code contracts (137 for kill, 124 for timeout, 4 for dep failure).

**Early ready signal**: When `--after` is set, the ready signal fires before the wait loop. This means `tender start` returns immediately with Starting state. Without `--after`, the existing late-signal flow is unchanged.

## Not in Scope

- Cross-namespace dependencies
- DAG visualization
- Automatic retry on dependency failure
- Fanout / parallel dispatch (separate plan)
- `tender run` directive for `--after` (can add later as `#tender: after <session>`)
