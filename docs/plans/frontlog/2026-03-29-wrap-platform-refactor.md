# Wrap Platform Refactor — Follow-up Plan

> **Context:** `tender wrap` (2026-03-28-wrap.md) shipped with Unix-only process control logic baked into `commands/wrap.rs`. This is acceptable as a scoped v0 fix, but not the architectural target. Windows is a first-class platform in this codebase; wrap must not become a second ad hoc supervision stack.

**Goal:** Move child lifecycle control out of `commands/wrap.rs` and behind the `Platform` trait, so wrap works on both Unix and Windows without platform-specific code in the command layer.

**Scope:** Refactor only. No new user-facing behavior. All existing tests must continue to pass.

---

## Design

### What wrap keeps (command-local)

- OS stop-notification detection (Unix: `libc::signal` + `AtomicBool`; Windows: `SetConsoleCtrlHandler`)
- Stdin buffering and piping
- Stdout/stderr capture and replay
- Annotation building and writing
- Exit code propagation

### What moves behind Platform

- Process-group / job-object spawn → already `Platform::spawn_child()`
- Graceful stop / force kill → already `Platform::kill_child(force: bool)`
- Non-blocking child polling → **new: `Platform::child_try_wait()`**

### Trait change

Add one method to `Platform` in `src/platform/mod.rs`:

```rust
/// Poll for child exit without blocking.
/// - `Ok(Some(status))`: child has exited.
/// - `Ok(None)`: child is still running.
/// - `Err(e)`: OS/backend wait failure.
fn child_try_wait(child: &mut Self::SupervisedChild) -> io::Result<Option<ExitStatus>>;
```

Tighten existing `kill_child` doc to make the graceful contract explicit:

```rust
/// Kill a supervised child via its kill handle.
///
/// - `force = false`: request graceful stop (SIGTERM on Unix,
///   CTRL_BREAK_EVENT or stop event on Windows); may wait for a
///   grace period and escalate internally.
/// - `force = true`: force immediate termination (SIGKILL / TerminateJobObject).
///
/// Does not reap the child. Callers must still call `child_wait`
/// or `child_try_wait` afterward.
fn kill_child(handle: &Self::ChildKillHandle, force: bool) -> io::Result<()>;
```

### Backend implementations

**Unix** (`src/platform/unix.rs`):

```rust
fn child_try_wait(child: &mut SupervisedChild) -> io::Result<Option<ExitStatus>> {
    child.child.try_wait()
}
```

**Windows** (`src/platform/windows.rs`):

Stub with `unsupported()` initially, same as other unimplemented methods. Real implementation via owned process handle when the Windows backend lands.

### Wrap flow after refactor

```rust
let mut child = Current::spawn_child(&cmd, true, None, &env)?;
let kill_handle = Current::child_kill_handle(&child);

// Take I/O handles via Platform
let stdout = Current::child_stdout(&mut child);
let stderr = Current::child_stderr(&mut child);
if let Some(mut stdin) = Current::child_stdin(&mut child) {
    let _ = stdin.write_all(&stdin_buf);
}

// Spawn capture threads for stdout/stderr...

// Poll loop
loop {
    if let Some(status) = Current::child_try_wait(&mut child)? {
        break status;
    }
    if stop_requested && !stop_forwarded {
        Current::kill_child(&kill_handle, false)?;
        stop_forwarded = true;
    }
    std::thread::sleep(POLL_INTERVAL);
}
```

This eliminates from `commands/wrap.rs`:
- `#[cfg(unix)]` on `pre_exec` / `setpgid` (spawn_child handles it)
- `rustix::process::kill_*` calls (kill_child handles it)
- `wait_for_child_with_sigterm` / `send_signal` / `send_signal_group` / `send_signal_direct`
- All `#[cfg(unix)]` / `#[cfg(not(unix))]` gates except the stop-notification handler

---

## Tasks

### Task 1: Add `child_try_wait` to Platform trait

Add the method signature to `src/platform/mod.rs`. Implement in `unix.rs` (delegate to `child.child.try_wait()`). Stub in `windows.rs`.

**Files:** `src/platform/mod.rs`, `src/platform/unix.rs`, `src/platform/windows.rs`

### Task 2: Tighten `kill_child` doc

Update the doc comment on `Platform::kill_child` to make the graceful/force contract explicit per the design above.

**Files:** `src/platform/mod.rs`

### Task 3: Refactor wrap to use Platform trait

Replace direct `Command::new` + `pre_exec` + `setpgid` with `Current::spawn_child()`. Replace signal sending with `Current::kill_child()`. Replace `child.try_wait()` with `Current::child_try_wait()`. Use `Current::child_stdout/stderr/stdin` for I/O handles.

Remove all Unix-specific process control code from `commands/wrap.rs`. The only `#[cfg]` blocks remaining should be the stop-notification handler (SIGTERM / `SetConsoleCtrlHandler`).

**Files:** `src/commands/wrap.rs`

### Task 4: Verify tests

All existing tests in `tests/cli_wrap.rs` must pass unchanged. The SIGTERM forwarding test (`wrap_forwards_sigterm_and_writes_annotation`) exercises the full path through the platform layer.

**Files:** `tests/cli_wrap.rs`

---

## Files Summary

| File | Change |
|------|--------|
| `src/platform/mod.rs` | Add `child_try_wait`, update `kill_child` doc |
| `src/platform/unix.rs` | Implement `child_try_wait` |
| `src/platform/windows.rs` | Stub `child_try_wait` |
| `src/commands/wrap.rs` | Remove direct process control, use `Platform` trait |

## Not In Scope

- Windows stop-notification handler (`SetConsoleCtrlHandler`) — deferred until Windows backend is real
- Changes to sidecar's use of the Platform trait
- New wrap-specific supervised child type
