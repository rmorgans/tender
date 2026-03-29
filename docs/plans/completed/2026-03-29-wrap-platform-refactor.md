# Wrap Platform Refactor — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Context:** `tender wrap` shipped with Unix-only process control logic baked into `commands/wrap.rs`. The Windows child lifecycle slice has since landed — both platforms now have real `spawn_child`, `child_try_wait`, `kill_child`, and full I/O. Time to move wrap onto the `Platform` trait.

**Goal:** Move child lifecycle control out of `commands/wrap.rs` and behind the `Platform` trait, so wrap works on both Unix and Windows without platform-specific process control code in the command layer.

**Scope:** Refactor only. No new user-facing behavior. All existing tests must continue to pass.

**Quality gates:** `cargo fmt` before each commit. `cargo clippy` on changed files. Full `cargo test` on macOS. `cargo check` on Windows (wrap CLI tests require sidecar which is not yet Windows-ready, but compilation must pass).

---

## Substrate (already landed)

These are done — do not re-implement:

| Method | Unix | Windows |
|--------|------|---------|
| `Platform::spawn_child` | Real (setpgid) | Real (CREATE_NEW_PROCESS_GROUP + Job Object) |
| `Platform::child_try_wait` | Real (try_wait) | Real (try_wait) |
| `Platform::kill_child(force)` | Real (SIGTERM→SIGKILL) | Real (CTRL_BREAK→TerminateJobObject) |
| `Platform::child_stdout/stderr/stdin` | Real | Real |
| `Platform::child_kill_handle` | Real | Real |
| `Platform::child_wait` | Real | Real |

---

## Design

### What wrap keeps (command-local)

- OS stop-notification detection (Unix: `libc::signal` + `AtomicBool`; Windows: `SetConsoleCtrlHandler` + `AtomicBool`)
- Stdin buffering and piping
- Stdout/stderr capture and replay
- Annotation building and writing
- Exit code propagation

### What gets removed from wrap

- `Command::new` + `pre_exec` / `setpgid` → replaced by `Current::spawn_child()`
- `send_signal` / `send_signal_group` / `send_signal_direct` → replaced by `Current::kill_child()`
- `wait_for_child_with_sigterm` polling loop → replaced by `Current::child_try_wait()` + `Current::kill_child()`
- All `#[cfg(unix)]` / `#[cfg(not(unix))]` gates on process control code

### What remains platform-specific in wrap

Only the stop-notification handler:
- Unix: `libc::signal(SIGTERM, handler)` sets `STOP_REQUESTED` AtomicBool
- Windows: `SetConsoleCtrlHandler` sets `STOP_REQUESTED` AtomicBool on CTRL_C/CTRL_BREAK/CTRL_CLOSE

Both sides feed the same `STOP_REQUESTED` flag. The poll loop is uniform.

### Wrap flow after refactor

```rust
use crate::platform::Current;
use tender::platform::Platform;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

// ... install_stop_handler() sets STOP_REQUESTED on SIGTERM (Unix) or
// CTRL_BREAK/CTRL_CLOSE (Windows) ...

let env = BTreeMap::new(); // wrap doesn't set extra env
let mut child = Current::spawn_child(&cmd, true, None, &env)
    .map_err(|e| anyhow::anyhow!("failed to spawn '{}': {e}", cmd[0]))?;
let kill_handle = Current::child_kill_handle(&child);

// Take I/O handles via Platform
let stdout = Current::child_stdout(&mut child);
let stderr = Current::child_stderr(&mut child);
if let Some(mut stdin) = Current::child_stdin(&mut child) {
    let _ = stdin.write_all(&stdin_buf);
    // Drop closes the pipe — child sees EOF
}

// Spawn capture threads for stdout/stderr (same as today)

// Poll loop — uniform across platforms
let mut stop_forwarded = false;
let status = loop {
    if let Some(status) = Current::child_try_wait(&mut child)? {
        break status;
    }
    if STOP_REQUESTED.load(Ordering::SeqCst) && !stop_forwarded {
        // kill_child(false) handles graceful→force escalation internally
        let _ = Current::kill_child(&kill_handle, false);
        stop_forwarded = true;
    }
    std::thread::sleep(POLL_INTERVAL);
};
```

Key simplification: `kill_child(false)` already handles the SIGTERM→SIGKILL / CTRL_BREAK→TerminateJobObject escalation internally with its own 5s grace period. Wrap doesn't need its own `kill_deadline` or two-phase signal logic anymore.

---

## Tasks

### Task 1: Refactor wrap spawn to use Platform trait

**Files:** `src/commands/wrap.rs`

**Step 1:** Replace imports. Remove:
```rust
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus};
```
Add:
```rust
use std::collections::BTreeMap;
use std::process::ExitStatus;
use tender::platform::{Current, Platform};
```

**Step 2:** Replace the child spawn block (lines 55-80) with:
```rust
if cmd.is_empty() {
    anyhow::bail!("no command specified");
}
let env = BTreeMap::new();
let mut child = Current::spawn_child(&cmd, true, None, &env)
    .map_err(|e| anyhow::anyhow!("failed to spawn '{}': {e}", cmd[0]))?;
```

**Step 3:** Replace stdin piping (lines 82-86) with:
```rust
if let Some(mut child_stdin) = Current::child_stdin(&mut child) {
    let _ = child_stdin.write_all(&stdin_buf);
    // Drop closes the pipe — child sees EOF
}
```

**Verify:** `cargo check` passes.

**Commit:** `refactor(wrap): use Platform::spawn_child instead of direct Command`

---

### Task 2: Refactor wait/signal handling to use Platform trait

**Files:** `src/commands/wrap.rs`

**Step 1:** Replace `wait_with_signal_handling` to use Platform I/O and polling:

```rust
fn wait_with_signal_handling(
    child: &mut <Current as Platform>::SupervisedChild,
    kill_handle: &<Current as Platform>::ChildKillHandle,
) -> io::Result<std::process::Output> {
    let stdout = Current::child_stdout(child);
    let stderr = Current::child_stderr(child);

    let stdout_handle = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut r) = stdout {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    let stderr_handle = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut r) = stderr {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    let mut stop_forwarded = false;
    let status = loop {
        if let Some(status) = Current::child_try_wait(child)? {
            break status;
        }
        if STOP_REQUESTED.load(Ordering::SeqCst) && !stop_forwarded {
            let _ = Current::kill_child(kill_handle, false);
            stop_forwarded = true;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    Ok(std::process::Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}
```

**Step 2:** Remove `wait_for_child_with_sigterm`, `send_signal`, `send_signal_group`, `send_signal_direct` — all four functions.

**Step 3:** Remove `SIGTERM_GRACE_PERIOD` and `SIGNAL_POLL_INTERVAL` constants. Add a single `POLL_INTERVAL`:
```rust
const POLL_INTERVAL: Duration = Duration::from_millis(50);
```

**Step 4:** Update `cmd_wrap` to pass `kill_handle` to `wait_with_signal_handling`.

**Verify:** `cargo check` passes.

**Commit:** `refactor(wrap): replace Unix signal handling with Platform::kill_child`

---

### Task 3: Add Windows stop-notification handler

**Files:** `src/commands/wrap.rs`

The Unix SIGTERM handler already exists. Add the Windows equivalent so wrap can detect stop requests on Windows too.

**Step 1:** Rename `install_sigterm_handler` and `sigterm_handler` to `install_stop_handler` and rename `SIGTERM_RECEIVED` to `STOP_REQUESTED` for platform-neutral naming.

**Step 2:** Add Windows handler:
```rust
#[cfg(windows)]
fn install_stop_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        STOP_REQUESTED.store(true, Ordering::SeqCst);
        1 // TRUE — handled
    }

    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}
```

**Step 3:** Update the `#[cfg(unix)]` block in `cmd_wrap` that calls the handler to be unconditional (both platforms now have a handler).

**Verify:** `cargo check` on macOS. `cargo check` on Windows via SSH.

**Commit:** `feat(wrap): add Windows stop-notification handler (SetConsoleCtrlHandler)`

---

### Task 4: Clean up dead code

**Files:** `src/commands/wrap.rs`

Remove any remaining:
- `#[cfg(unix)]` / `#[cfg(not(unix))]` gates on process control (stop handler `#[cfg]` stays — that's expected)
- Unused imports (`libc`, `rustix::process::*`, `std::os::unix::process::CommandExt`, `Instant`)
- Dead constants

**Verify:** `cargo clippy -- -W clippy::all` clean on `commands/wrap.rs`.

**Commit:** `chore(wrap): remove dead Unix-only process control code`

---

### Task 5: Verify all tests pass

Run full `cargo test` on macOS. All existing tests in `tests/cli_wrap.rs` must pass unchanged, including `wrap_forwards_sigterm_and_writes_annotation` which exercises the full SIGTERM→child path through the platform layer.

Run `cargo check` on Windows to confirm compilation.

Note: `wrap_forwards_sigterm_and_writes_annotation` is `#[cfg(unix)]` — this is correct and expected. The Windows equivalent would require a different signal mechanism (tested separately via `windows_kill_graceful_cooperative`).

**Commit:** No commit unless test fixes are needed.

---

## Files Summary

| File | Change |
|------|--------|
| `src/commands/wrap.rs` | Replace direct process control with Platform trait, add Windows stop handler, remove dead code |

## Not In Scope

- Windows CLI-level wrap tests (requires sidecar spawn on Windows — separate slice)
- Changes to sidecar's use of the Platform trait
- Changing annotation format or truncation logic
- New wrap-specific supervised child type
