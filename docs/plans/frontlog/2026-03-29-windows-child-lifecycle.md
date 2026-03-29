# Windows Child Lifecycle — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement the Windows `Platform` trait methods for supervised child lifecycle: spawn, I/O, wait, try_wait, kill. This makes child process management real on Windows, not stubbed.

**Architecture:** Use `windows_sys` (already a dependency) for Win32 APIs. Spawn child via `std::process::Command` with `CREATE_NEW_PROCESS_GROUP`. Assign child to a Job Object immediately after spawn for tree kill. Force kill via `TerminateJobObject`. Add `child_try_wait` to the `Platform` trait.

**Tech Stack:** Rust, `windows_sys` 0.61, `std::process::Command`.

**Quality gates:** `cargo fmt` before each commit. `cargo clippy` on changed files. Full `cargo test` on both macOS and `rick-windows` (via SSH at `100.90.60.48`).

**Scope:** Platform-level child lifecycle only. Tests exercise `Platform` methods directly, not through `tender start` CLI (which requires sidecar spawn, readiness channel, and other unimplemented Windows pieces).

**NOT in scope:** sidecar spawn, readiness channel, stdin transport, `kill_orphan`, pre-existing session_fs test failures.

---

## Design Decisions (locked)

| # | Decision | Choice |
|---|----------|--------|
| 0 | Spawn mechanism | `std::process::Command` with `CREATE_NEW_PROCESS_GROUP`. No `CREATE_SUSPENDED` — `std::process::Child` does not expose the thread handle needed for `ResumeThread`. Job Object assigned immediately after spawn. |
| 1 | Process containment | Win32 Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Safety net: if the owning process dies, all children in the job die too. |
| 2 | Spawn race window | Acknowledged: child runs briefly before Job Object assignment. Children spawned in that window escape the job. Acceptable for v0 — the race is sub-millisecond and `KILL_ON_JOB_CLOSE` catches the common case. Upgrade path: raw `CreateProcessW` with `CREATE_SUSPENDED` in a follow-up if needed. |
| 3 | Force kill | `TerminateJobObject(handle, 1)` — kills the entire job tree immediately. |
| 4 | Graceful stop contract | Best-effort `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)`. Degrades to force kill if: (a) child has no console, (b) child ignores CTRL_BREAK, or (c) child doesn't exit within 5s grace period. This is the explicit contract — graceful stop on Windows is not guaranteed. |
| 5 | Wait | Delegates to `std::process::Child::wait()` and `try_wait()`. Rust std handles `WaitForSingleObject` internally. |
| 6 | Handle ownership | `SupervisedChild` owns `std::process::Child` + `Arc<OwnedHandle>` (Job Object). `ChildKillHandle` clones the `Arc<OwnedHandle>` + `ProcessIdentity`. Arc avoids `DuplicateHandle` complexity while keeping `Clone + Send`. |
| 7 | New trait method | `child_try_wait` added to `Platform` trait. Uniform interface for non-blocking child polling on both platforms. |

---

## Per-Slice Invariant Table

| Invariant | Why it matters | Enforced by | Tested by | Known exceptions |
|-----------|---------------|-------------|-----------|-----------------|
| Child assigned to Job Object immediately after spawn | Tree kill covers the child and most descendants | `assign_process_to_job` called right after `cmd.spawn()` | `windows_spawn_assigns_job_object` | Sub-ms race: children spawned before assignment escape |
| Job Object has `KILL_ON_JOB_CLOSE` | Safety net — owning process death kills orphans | `SetInformationJobObject` in `create_job_object()` | `windows_job_object_kill_on_close` | None |
| `ChildKillHandle` is Send + Clone | Timeout thread needs it | `Arc<OwnedHandle>` + `ProcessIdentity: Copy` | Compile-time trait bound | None |
| Graceful kill degrades to force after 5s | No indefinite hang | `kill_child(false)` polls → escalates to `TerminateJobObject` | `windows_kill_graceful_escalates` | None |
| `child_try_wait` returns `None` while running | Polling loop in wrap depends on this | Delegates to `std::process::Child::try_wait()` | `windows_try_wait_while_running` | None |
| Force kill is idempotent | Double-kill must not error | `TerminateJobObject` error suppressed for already-dead jobs | `windows_force_kill_idempotent` | None |

---

## Tasks

### Task 0: Add `child_try_wait` to Platform trait

**Files:** `src/platform/mod.rs`, `src/platform/unix.rs`, `src/platform/windows.rs`

**Step 1:** Add to `Platform` trait in `src/platform/mod.rs`, after `child_wait`:

```rust
/// Poll for child exit without blocking.
/// Returns `Ok(Some(status))` if the child has exited, `Ok(None)` if still
/// running, or `Err` on OS failure.
fn child_try_wait(child: &mut Self::SupervisedChild) -> io::Result<Option<ExitStatus>>;
```

**Step 2:** Update `kill_child` doc in the same file:

```rust
/// Kill a supervised child via its kill handle.
///
/// - `force = false`: request graceful stop (SIGTERM on Unix,
///   best-effort CTRL_BREAK on Windows); waits up to 5s then
///   escalates to force termination.
/// - `force = true`: immediate termination (SIGKILL / TerminateJobObject).
///
/// Does not reap the child. Callers must still call `child_wait`
/// or `child_try_wait` afterward.
fn kill_child(handle: &Self::ChildKillHandle, force: bool) -> io::Result<()>;
```

**Step 3:** Implement in `src/platform/unix.rs`:

```rust
fn child_try_wait(child: &mut SupervisedChild) -> io::Result<Option<ExitStatus>> {
    child.child.try_wait()
}
```

**Step 4:** Stub in `src/platform/windows.rs`:

```rust
fn child_try_wait(_child: &mut SupervisedChild) -> io::Result<Option<ExitStatus>> {
    Err(unsupported("child_try_wait"))
}
```

**Step 5:** Run `cargo test` on macOS — all existing tests must pass.

**Commit:** `feat: add Platform::child_try_wait for non-blocking child polling`

---

### Task 1: Add windows_sys features to Cargo.toml

**Files:** `Cargo.toml`

Update the `windows-sys` dependency:

```toml
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_System_Console",
    "Win32_System_IO",
    "Win32_System_JobObjects",
    "Win32_System_Threading",
    "Win32_Storage_FileSystem",
] }
```

New features: `Win32_System_Console` (`GenerateConsoleCtrlEvent`), `Win32_System_JobObjects` (`CreateJobObjectW`, `AssignProcessToJobObject`, `TerminateJobObject`, `SetInformationJobObject`).

**Verify:** `cargo check` passes on macOS.

**Commit:** `chore: add windows_sys features for Job Objects and console control`

---

### Task 2: Implement `SupervisedChild`, `spawn_child`, and helpers

**Files:** `src/platform/windows.rs`

**Step 1:** Add imports:

```rust
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::sync::Arc;
```

**Step 2:** Replace placeholder `SupervisedChild`:

```rust
pub struct SupervisedChild {
    child: std::process::Child,
    identity: ProcessIdentity,
    job_object: Arc<OwnedHandle>,
}
```

**Step 3:** Implement `spawn_child`:

```rust
fn spawn_child(
    argv: &[String],
    stdin_piped: bool,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> io::Result<SupervisedChild> {
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    if stdin_piped {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if !env.is_empty() {
        cmd.envs(env);
    }

    // CREATE_NEW_PROCESS_GROUP: required for GenerateConsoleCtrlEvent targeting.
    // No CREATE_SUSPENDED — std::process::Child doesn't expose the thread
    // handle needed for ResumeThread. See Decision #2 for the race trade-off.
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    let child = cmd.spawn()?;

    let job = create_job_object()?;

    // Assign child to Job Object immediately after spawn.
    // Race window: child may briefly run before assignment. See Decision #2.
    assign_process_to_job(
        job.as_raw_handle() as isize,
        child.as_raw_handle() as isize,
    )?;

    let pid = child.id();
    let identity = process_identity(pid)?;

    Ok(SupervisedChild {
        child,
        identity,
        job_object: Arc::new(job),
    })
}
```

**Step 4:** Add helpers:

```rust
/// Create a Job Object with KILL_ON_JOB_CLOSE (safety net for crashes).
fn create_job_object() -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    // SAFETY: null name = anonymous job object. Returns null on failure.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }

    // Configure kill-on-close before returning, while we still have the raw handle.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    let ret = unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ret == 0 {
        // Close the job handle before returning the error.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(io::Error::last_os_error());
    }

    // SAFETY: handle is a valid non-null HANDLE from CreateJobObjectW.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut _) })
}

/// Assign a process to a Job Object.
fn assign_process_to_job(job: isize, process: isize) -> io::Result<()> {
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    // SAFETY: both handles are valid — job from create_job_object,
    // process from std::process::Child (which owns the process HANDLE).
    let ret = unsafe { AssignProcessToJobObject(job, process) };
    if ret == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
```

**Verify:** `cargo check` on macOS (windows.rs is only compiled on Windows, so this checks syntax of non-cfg'd code).

**Commit:** `feat(windows): implement spawn_child with Job Object containment`

---

### Task 3: Implement child I/O, wait, try_wait, identity

**Files:** `src/platform/windows.rs`

Replace the stubs:

```rust
fn child_identity(child: &SupervisedChild) -> io::Result<ProcessIdentity> {
    Ok(child.identity)
}

fn child_wait(child: &mut SupervisedChild) -> io::Result<ExitStatus> {
    child.child.wait()
}

fn child_try_wait(child: &mut SupervisedChild) -> io::Result<Option<ExitStatus>> {
    child.child.try_wait()
}

fn child_stdout(child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
    child.child.stdout.take()
        .map(|s| Box::new(s) as Box<dyn io::Read + Send>)
}

fn child_stderr(child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
    child.child.stderr.take()
        .map(|s| Box::new(s) as Box<dyn io::Read + Send>)
}

fn child_stdin(child: &mut SupervisedChild) -> Option<Box<dyn io::Write + Send>> {
    child.child.stdin.take()
        .map(|s| Box::new(s) as Box<dyn io::Write + Send>)
}
```

Also update `child_kill_handle` — see Task 4.

**Verify:** `cargo check` on macOS.

**Commit:** `feat(windows): implement child I/O, wait, try_wait, identity`

---

### Task 4: Implement `ChildKillHandle` and `kill_child`

**Files:** `src/platform/windows.rs`

**Step 1:** Replace `ChildKillHandle`:

```rust
/// Lightweight kill handle for Windows.
/// Carries an Arc'd Job Object HANDLE for tree kill and ProcessIdentity
/// for status checks and GenerateConsoleCtrlEvent targeting.
#[derive(Clone)]
pub struct ChildKillHandle {
    identity: ProcessIdentity,
    job_object: Arc<OwnedHandle>,
}
```

**Step 2:** Implement `child_kill_handle`:

```rust
fn child_kill_handle(child: &SupervisedChild) -> ChildKillHandle {
    ChildKillHandle {
        identity: child.identity,
        job_object: child.job_object.clone(),
    }
}
```

**Step 3:** Implement `kill_child`:

```rust
fn kill_child(handle: &ChildKillHandle, force: bool) -> io::Result<()> {
    if force {
        return terminate_job(&handle.job_object);
    }

    // Graceful: best-effort CTRL_BREAK, then poll, then escalate.
    // Contract: degrades to force if child has no console or ignores the signal.
    send_ctrl_break(handle.identity.pid.get());

    for _ in 0..50 {
        match process_status(&handle.identity) {
            ProcessStatus::Missing | ProcessStatus::IdentityMismatch => return Ok(()),
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Grace period expired — escalate to force.
    terminate_job(&handle.job_object)
}
```

**Step 4:** Add helpers:

```rust
/// Terminate all processes in a Job Object. Idempotent — already-dead is Ok.
fn terminate_job(job: &OwnedHandle) -> io::Result<()> {
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    let ret = unsafe { TerminateJobObject(job.as_raw_handle() as isize, 1) };
    if ret == 0 {
        let err = io::Error::last_os_error();
        // ERROR_ACCESS_DENIED can mean the job is already terminated.
        if err.raw_os_error()
            != Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
        {
            return Err(err);
        }
    }
    Ok(())
}

/// Best-effort CTRL_BREAK to a process group. No-op if the child has no console.
fn send_ctrl_break(pid: u32) {
    use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
    // The child was created with CREATE_NEW_PROCESS_GROUP, so pid == group id.
    // Failure is silently ignored — this is a best-effort graceful stop.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
}
```

**Verify:** `cargo check` on macOS.

**Commit:** `feat(windows): implement kill_child with best-effort CTRL_BREAK and Job Object termination`

---

### Task 5: Platform-level tests on Windows

Tests exercise `Platform` methods directly — no dependency on sidecar, readiness, or CLI path. All tests are `#[cfg(windows)]`.

**Files:** Create `tests/windows_child.rs`

```rust
#![cfg(windows)]

use std::collections::BTreeMap;
use std::io::Read;
use tender::platform::windows::WindowsPlatform;
use tender::platform::Platform;

/// Spawn a child, verify it gets a valid identity.
#[test]
fn windows_spawn_child_identity() {
    let argv = vec!["cmd".into(), "/C".into(), "echo hello".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    let id = WindowsPlatform::child_identity(&child).expect("identity should be available");
    assert!(id.pid.get() > 0);
    assert!(id.start_time_ns > 0);

    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(status.success());
}

/// Spawn, verify stdout is captured.
#[test]
fn windows_spawn_captures_stdout() {
    let argv = vec!["cmd".into(), "/C".into(), "echo hello-windows".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    let mut stdout = WindowsPlatform::child_stdout(&mut child)
        .expect("stdout should be available");

    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(status.success());

    let mut buf = String::new();
    stdout.read_to_string(&mut buf).unwrap();
    assert!(buf.contains("hello-windows"), "stdout should contain output, got: {buf}");
}

/// try_wait returns None while child is running, Some after exit.
#[test]
fn windows_try_wait_while_running() {
    let argv = vec!["cmd".into(), "/C".into(), "timeout /t 10 /nobreak".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    // Child should still be running
    let poll = WindowsPlatform::child_try_wait(&mut child).expect("try_wait should not error");
    assert!(poll.is_none(), "child should still be running");

    // Force kill it
    let handle = WindowsPlatform::child_kill_handle(&child);
    WindowsPlatform::kill_child(&handle, true).expect("force kill should succeed");

    // Wait for exit
    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(!status.success(), "force-killed child should have non-zero exit");
}

/// Force kill terminates the child immediately.
#[test]
fn windows_force_kill() {
    let argv = vec!["cmd".into(), "/C".into(), "timeout /t 60 /nobreak".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    let handle = WindowsPlatform::child_kill_handle(&child);
    WindowsPlatform::kill_child(&handle, true).expect("force kill should succeed");

    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(!status.success());
}

/// Force kill is idempotent — killing an already-dead process is Ok.
#[test]
fn windows_force_kill_idempotent() {
    let argv = vec!["cmd".into(), "/C".into(), "echo done".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    let handle = WindowsPlatform::child_kill_handle(&child);
    WindowsPlatform::child_wait(&mut child).expect("wait should succeed");

    // Kill again — child is already dead, should not error.
    WindowsPlatform::kill_child(&handle, true).expect("double kill should be idempotent");
}

/// Graceful kill sends CTRL_BREAK, escalates to force after timeout.
#[test]
fn windows_kill_graceful_escalates() {
    let argv = vec!["cmd".into(), "/C".into(), "timeout /t 60 /nobreak".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    let handle = WindowsPlatform::child_kill_handle(&child);

    // Graceful kill — timeout /t 60 /nobreak ignores CTRL_BREAK,
    // so this should escalate to TerminateJobObject after ~5s.
    WindowsPlatform::kill_child(&handle, false).expect("graceful kill should succeed");

    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(!status.success(), "escalated kill should produce non-zero exit");
}

/// Stdin piping works.
#[test]
fn windows_spawn_with_stdin() {
    use std::io::Write;

    // `findstr .` reads stdin and echoes lines that match "." (i.e., everything)
    let argv = vec!["findstr".into(), ".".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, true, None, &BTreeMap::new())
        .expect("spawn_child should succeed");

    let mut stdin = WindowsPlatform::child_stdin(&mut child)
        .expect("stdin should be available");
    stdin.write_all(b"hello from stdin\n").unwrap();
    drop(stdin); // close pipe — child sees EOF

    let mut stdout = WindowsPlatform::child_stdout(&mut child)
        .expect("stdout should be available");

    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(status.success());

    let mut buf = String::new();
    stdout.read_to_string(&mut buf).unwrap();
    assert!(buf.contains("hello from stdin"), "stdout should echo stdin, got: {buf}");
}

/// Environment variables are passed to child.
#[test]
fn windows_spawn_with_env() {
    let mut env = BTreeMap::new();
    env.insert("TENDER_TEST_VAR".into(), "hello-env".into());

    let argv = vec!["cmd".into(), "/C".into(), "echo %TENDER_TEST_VAR%".into()];
    let mut child = WindowsPlatform::spawn_child(&argv, false, None, &env)
        .expect("spawn_child should succeed");

    let mut stdout = WindowsPlatform::child_stdout(&mut child)
        .expect("stdout should be available");

    let status = WindowsPlatform::child_wait(&mut child).expect("wait should succeed");
    assert!(status.success());

    let mut buf = String::new();
    stdout.read_to_string(&mut buf).unwrap();
    assert!(buf.contains("hello-env"), "env var should be visible, got: {buf}");
}
```

**Note on visibility:** The tests import `tender::platform::windows::WindowsPlatform` directly. This requires `WindowsPlatform` and the Platform trait to be `pub`. Check that `src/platform/mod.rs` and `src/platform/windows.rs` have the right visibility — they should, since the types are already `pub`.

**Run on Windows:** `ssh rick@100.90.60.48` → clone repo → `cargo test --test windows_child`

**Commit:** `test(windows): platform-level child lifecycle tests`

---

## Progress

Tasks 0 and 1 completed on macOS (`36bd251`, `a8c4456`, `f2047ae` — pushed to main).

### Windows session checklist

1. `git pull` to pick up Tasks 0-1
2. Implement Tasks 2-4 in `src/platform/windows.rs`
3. Add `tests/windows_child.rs` (Task 5)
4. `cargo test --test windows_child` — all 8 new tests green
5. Run existing Windows-relevant integration tests (`cargo test`) — surface any gaps in cli_kill, sidecar_child, sidecar_ready, etc.
6. Review containment/graceful-kill behavior before calling slice done

If Task 2's spawn race (Option A) breaks any containment test, stop and revise the plan.

---

## Files Summary

| File | Change |
|------|--------|
| `src/platform/mod.rs` | Add `child_try_wait`, update `kill_child` doc |
| `src/platform/unix.rs` | Implement `child_try_wait` (trivial) |
| `src/platform/windows.rs` | Real `SupervisedChild`, `spawn_child`, I/O, wait, try_wait, kill |
| `Cargo.toml` | Add `Win32_System_JobObjects`, `Win32_System_Console` features |
| `tests/windows_child.rs` | **New** — 8 platform-level tests |

## Not In Scope

- Sidecar spawn on Windows (`DETACHED_PROCESS` + readiness pipe) — separate slice
- Readiness channel (anonymous pipe on Windows) — separate slice
- Stdin transport (named pipes `\\.\pipe\tender-<session>`) — separate slice
- `kill_orphan` (no Job Object handle for orphans) — separate slice
- `CREATE_SUSPENDED` spawn — follow-up if race window proves problematic
- Wrap platform refactor — next slice after this one
- Pre-existing session_fs test failures — tracked separately, not a blocker for this slice
