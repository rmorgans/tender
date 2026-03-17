# Slice 8A: Wait, Reconciliation, Idempotent Start Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Complete Phase 1 behaviorally: `tender wait` for blocking on session completion, SidecarLost reconciliation for crash recovery, idempotent `tender start` with spec matching, and `--replace` for explicit restart.

**Architecture:** `wait` polls meta.json until terminal state, with optional timeout. Reconciliation runs opportunistically during `status` and `wait` — if the session lock is not held and meta shows non-terminal, the sidecar crashed and we write SidecarLost. For sessions with `child_pid` but no `meta.json` (sidecar crashed between spawn and meta write), reconciliation kills the orphan and cleans up the dir. Idempotent start compares canonical_hash of the new LaunchSpec against the existing session's; match → return existing, mismatch → error. `--replace` kills the existing session, waits for the sidecar to fully exit (terminal state + lock released), then removes the dir and starts fresh.

**Tech Stack:** Rust std only. No new dependencies.

**Key design decisions:**
- Reconciliation is triggered by `status` and `wait`, not by a background daemon. Agents calling `status` after a crash will see the correct state.
- Idempotent start checks hash, not field-by-field comparison. The hash is SHA-256 of canonical JSON.
- `--replace` waits for positive handoff: terminal state + lock released. Does NOT race-delete while old sidecar may still be writing.
- Generation is always 1 in 8A. Generation increment on replace deferred to 8B (it's a debug counter, not used for lifecycle decisions).
- Wait exit codes: 0 for ExitedOk/Killed/KilledForced/TimedOut, 2 for SpawnFailed (matches start's convention), 3 for SidecarLost, 42 for ExitedError.

---

## Task 1: Wait command

**Files:**
- Modify: `src/main.rs`

Add `Wait` variant to `Commands`:
```rust
/// Block until session reaches terminal state
Wait {
    /// Session name
    name: String,
    /// Timeout in seconds
    #[arg(short, long)]
    timeout: Option<u64>,
},
```

Add match arm and implement `cmd_wait`:
```rust
fn cmd_wait(name: &str, timeout: Option<u64>) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::model::state::RunStatus;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let deadline = timeout.map(|t| std::time::Instant::now() + std::time::Duration::from_secs(t));

    loop {
        let meta = session::read_meta(&session)?;

        // Reconciliation: non-terminal + lock not held → sidecar crashed
        if !meta.status().is_terminal() && !session::is_locked(&session)? {
            let mut meta = meta;
            meta.reconcile_sidecar_lost(now_epoch_secs())?;
            session::write_meta_atomic(&session, &meta)?;
            // Re-read to get the reconciled state and fall through to terminal check
            continue;
        }

        if meta.status().is_terminal() {
            let json = serde_json::to_string_pretty(&meta)?;
            println!("{json}");

            // Exit code follows convention
            match meta.status() {
                RunStatus::Exited { how, .. } => {
                    use tender::model::state::ExitReason;
                    match how {
                        ExitReason::ExitedOk => return Ok(()),
                        ExitReason::ExitedError { .. } => std::process::exit(42),
                        _ => return Ok(()), // Killed, KilledForced, TimedOut
                    }
                }
                RunStatus::SpawnFailed { .. } => std::process::exit(2),
                RunStatus::SidecarLost { .. } => std::process::exit(3),
                _ => return Ok(()),
            }
        }

        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                anyhow::bail!("timeout waiting for session {name}");
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
```

Add a shared `now_epoch_secs` helper in main.rs (duplicated from sidecar.rs since it's trivial):
```rust
fn now_epoch_secs() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", duration.as_secs())
}
```

Note: `session::is_locked` doesn't exist yet — it's added in Task 2. Tasks 1 and 2 must be implemented together or Task 1 must compile-gate the reconciliation behind Task 2. Simplest: implement `is_locked` first as part of Task 1, then use it in both wait and status.

**Tests:** `tests/cli_wait.rs`
1. `wait_returns_terminal_state`: start `true`, wait → prints Exited JSON with ExitedOk, exit 0
2. `wait_blocks_until_exit`: start `sleep 2`, wait → blocks ~2s, then returns
3. `wait_timeout_expires`: start `sleep 60`, wait --timeout 1 → non-zero exit, returns in ~1s not 60s. Clean up: kill the sleep.
4. `wait_nonexistent_session_fails`: wait nope → error
5. `wait_exit_code_42_for_nonzero_child`: start `sh -c "exit 3"`, wait → exit 42
6. `wait_exit_code_2_for_spawn_failed`: start `/nonexistent/binary`, wait → exit 2

**Commit:** `feat: add tender wait command with timeout and exit code convention`

---

## Task 2: SidecarLost reconciliation in status + orphan cleanup

**Files:**
- Modify: `src/main.rs` (cmd_status)
- Modify: `src/session.rs` (add `is_locked` helper)

### 2a: Add is_locked to session.rs

```rust
/// Check if the session lock is currently held by another process.
/// Returns true if locked, false if available. Does not acquire.
pub fn is_locked(session: &SessionDir) -> Result<bool, SessionError> {
    let lock_path = session.lock_path();
    if !lock_path.exists() {
        return Ok(false); // no lock file = not locked
    }
    match LockGuard::try_acquire(session) {
        Ok(_guard) => Ok(false),  // We got it → was not locked. Drop releases.
        Err(SessionError::Locked(_)) => Ok(true),
        Err(e) => Err(e),
    }
}
```

### 2b: Reconciliation in cmd_status

```rust
fn cmd_status(name: &str) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Try normal open first
    let session = match session::open(&root, &session_name)? {
        Some(s) => s,
        None => {
            // Check for orphan dir (child_pid exists but no meta.json)
            let orphan_dir = root.path().join(session_name.as_str());
            if orphan_dir.exists() {
                cleanup_orphan_dir(&orphan_dir);
                anyhow::bail!("session {name} was orphaned (sidecar crashed before writing state). Cleaned up.");
            }
            anyhow::bail!("session not found: {name}");
        }
    };

    let mut meta = session::read_meta(&session)?;

    // Reconciliation: non-terminal state + lock not held → sidecar crashed
    if !meta.status().is_terminal() && !session::is_locked(&session)? {
        meta.reconcile_sidecar_lost(now_epoch_secs())?;
        session::write_meta_atomic(&session, &meta)?;
    }

    let json = serde_json::to_string_pretty(&meta)?;
    println!("{json}");
    Ok(())
}
```

### 2c: Orphan dir cleanup helper

For dirs that have `child_pid` but no `meta.json` — sidecar crashed between child spawn and meta write:

```rust
/// Clean up an orphaned session dir that has child_pid but no meta.json.
/// Kills the orphaned child process if still alive, then removes the dir.
fn cleanup_orphan_dir(dir: &std::path::Path) {
    use tender::platform::unix as platform;

    // Try to kill orphaned child
    let child_pid_path = dir.join("child_pid");
    if let Ok(pid_str) = std::fs::read_to_string(&child_pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            // Best-effort kill — we don't have identity, so PID reuse safety is degraded.
            // This is acceptable because we're cleaning up a crash, not normal operation.
            unsafe { libc::kill(pid as i32, libc::SIGKILL); }
        }
    }

    let _ = std::fs::remove_dir_all(dir);
}
```

**Tests:** `tests/cli_reconcile.rs`
1. `status_reconciles_crashed_sidecar`: Start `sleep 60`, read sidecar PID from meta, kill -9 the sidecar directly, then `tender status` → shows SidecarLost
2. `status_does_not_reconcile_running_session`: Start `sleep 60`, status → Running (sidecar holds lock). Clean up: kill.
3. `wait_reconciles_crashed_sidecar`: Start `sleep 60`, kill -9 the sidecar, then `tender wait --timeout 5` → returns SidecarLost, exit 3

**Commit:** `feat: SidecarLost reconciliation in status/wait, orphan dir cleanup`

---

## Task 3: Idempotent start

**Files:**
- Modify: `src/main.rs` (cmd_start)

When `session::create` returns `AlreadyExists`:
1. Try normal `open` first. If that fails (no meta.json = orphan), clean up the orphan dir and retry create.
2. Read meta from the existing session.
3. If terminal → error: "session already exists in terminal state (use --replace to restart)"
4. If non-terminal → compare `canonical_hash`:
   - Match → return existing meta as JSON, exit 0 (idempotent)
   - Mismatch → error: "session conflict: different launch spec (use --replace to override)"

```rust
let session = match session::create(&root, &session_name) {
    Ok(s) => s,
    Err(session::SessionError::AlreadyExists(_)) => {
        // Check for orphan dir first
        let session_path = root.path().join(session_name.as_str());
        if !session_path.join("meta.json").exists() {
            cleanup_orphan_dir(&session_path);
            // Retry create after cleanup
            session::create(&root, &session_name)?
        } else {
            let existing = session::open(&root, &session_name)?
                .ok_or_else(|| anyhow::anyhow!("session dir exists but not openable"))?;
            let existing_meta = session::read_meta(&existing)?;

            if !existing_meta.status().is_terminal() {
                if existing_meta.launch_spec_hash() == launch_spec.canonical_hash() {
                    let json = serde_json::to_string_pretty(&existing_meta)?;
                    println!("{json}");
                    return Ok(());
                } else {
                    anyhow::bail!(
                        "session conflict: {name} is running with a different launch spec (use --replace to override)"
                    );
                }
            } else {
                anyhow::bail!(
                    "session already exists in terminal state: {name} (use --replace to restart)"
                );
            }
        }
    }
    Err(e) => return Err(e.into()),
};
```

**Tests:** `tests/cli_start_idempotent.rs`
1. `start_same_spec_is_idempotent`: start `sleep 60`, start again with same args → exit 0, returns existing meta with same run_id. Clean up: kill.
2. `start_different_spec_is_conflict`: start `sleep 60`, start `echo hi` with same name → non-zero exit, stderr contains "session conflict". Clean up: kill.
3. `start_after_terminal_is_error`: start `true`, wait terminal, start again → non-zero exit, stderr contains "terminal state"

**Commit:** `feat: idempotent start with launch-spec hash matching`

---

## Task 4: --replace flag

**Files:**
- Modify: `src/main.rs` (cmd_start)

Add `--replace` flag to `Start`:
```rust
/// Replace existing session (kill + restart)
#[arg(long)]
replace: bool,
```

The safe --replace sequence:
1. Open existing session (handle orphan dirs with cleanup_orphan_dir)
2. If non-terminal:
   a. Kill child (force kill)
   b. Wait for meta to reach terminal state (sidecar writes it)
   c. Wait for lock to be released (sidecar has fully exited)
   d. If sidecar doesn't exit within 10s, it's stuck — proceed anyway
3. Remove session dir entirely (`remove_dir_all`)
4. Proceed with normal start flow

```rust
if replace {
    let session_path = root.path().join(session_name.as_str());
    if session_path.exists() {
        // Handle orphan dir (no meta.json)
        if !session_path.join("meta.json").exists() {
            cleanup_orphan_dir(&session_path);
        } else if let Some(existing) = session::open(&root, &session_name)? {
            let existing_meta = session::read_meta(&existing)?;
            if !existing_meta.status().is_terminal() {
                // Kill child
                if let Some(child) = existing_meta.status().child() {
                    let _ = platform::kill_process(child, true);
                }
                // Wait for terminal state + lock release (sidecar fully exited)
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    if let Ok(m) = session::read_meta(&existing) {
                        if m.status().is_terminal() && !session::is_locked(&existing)? {
                            break;
                        }
                    }
                    if std::time::Instant::now() >= deadline {
                        break; // Sidecar stuck — proceed anyway
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            // Now safe to remove — sidecar has exited or timed out
            std::fs::remove_dir_all(existing.path())?;
        }
    }
}
```

This goes before the `session::create` call. After removal, the normal create + sidecar spawn path runs.

**Tests:** `tests/cli_replace.rs`
1. `replace_running_session`: start `sleep 60`, start --replace `echo replaced` → kills old, starts new. Status shows new session. Old child is dead.
2. `replace_terminal_session`: start `true`, wait terminal, start --replace `sleep 60` → starts fresh. Clean up: kill.
3. `replace_nonexistent_is_noop`: start --replace `echo hi` on new name → works like normal start

**Commit:** `feat: add --replace flag with safe sidecar handoff`

---

## Task 5: Full suite verification

Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. All green.

---

## Summary

| Task | What | Tests |
|------|------|-------|
| 1 | Wait command + is_locked | 6 (cli_wait.rs) |
| 2 | SidecarLost reconciliation + orphan cleanup | 3 (cli_reconcile.rs) |
| 3 | Idempotent start | 3 (cli_start_idempotent.rs) |
| 4 | --replace with safe handoff | 3 (cli_replace.rs) |
| 5 | Verification | 0 |

**Total new tests:** ~15 integration
**Modified files:** `src/main.rs`, `src/session.rs`
**New files:** `tests/cli_wait.rs`, `tests/cli_reconcile.rs`, `tests/cli_start_idempotent.rs`, `tests/cli_replace.rs`
**No new dependencies.**
**Deferred to 8B:** generation increment on replace, KilledForced/TimedOut reachability, typed timestamps, degradation visibility, push hang mitigation, typed readiness snapshot in cmd_start.
