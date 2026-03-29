---
id: windows-full-backend
title: "Windows Full Backend"
created: 2026-03-30
closed:
depends_on: []
links:
  - ../backlog/windows-full-backend.md
---

# Windows Full Backend — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Complete the Windows platform implementation so all tender CLI commands work end-to-end on Windows — `start`, `kill`, `status`, `list`, `watch`, `log`, `push`, `wrap`.

**Architecture:** Three slices building on the child lifecycle already landed. Slice 1 (sidecar spawn + readiness) is the main unlock. Slice 2 (stdin transport via named pipes) enables `push`. Slice 3 (orphan kill) completes parity. Each slice has named CLI tests as verification gates.

**Tech Stack:** Rust, `windows_sys` 0.61, `CreatePipe`, `CreateNamedPipeW`, `DETACHED_PROCESS`, `SetHandleInformation`.

**Quality gates:** `cargo fmt` before each commit. `cargo clippy` on changed files. `cargo check` on macOS (cross-check). Full `cargo test` on Windows per slice.

---

## Substrate (already landed)

| Method | Status |
|--------|--------|
| `spawn_child` | Real — `CREATE_NEW_PROCESS_GROUP` + Job Object |
| `child_try_wait` / `child_wait` | Real |
| `kill_child` | Real — CTRL_BREAK → WaitForSingleObject → TerminateJobObject |
| `child_stdout/stderr/stdin` | Real |
| `child_kill_handle` | Real — Arc'd Job Object |
| `child_identity` / `process_identity` / `process_status` | Real |
| `self_identity` | Real |
| `seal_ready_fd` | No-op (correct — Windows uses inheritable HANDLE marking) |

---

## Slice 1: Sidecar Spawn + Readiness

**Unlocks:** `tender start`, and therefore `status`, `list`, `watch`, `log`, `kill`, `wrap` (all CLI commands except `push`).

### Design Decisions

| # | Decision | Choice |
|---|----------|--------|
| 0 | Anonymous pipe | `CreatePipe` with `SECURITY_ATTRIBUTES.bInheritHandle = TRUE` on write end. Read end stays non-inheritable (parent only). |
| 1 | Handle passing | Pass write HANDLE value as string in `TENDER_READY_HANDLE` env var (same pattern as Unix's `TENDER_READY_FD`). Simpler than `STARTUPINFOEX` + `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. |
| 2 | Sidecar detachment | `CREATE_NEW_PROCESS_GROUP` + `DETACHED_PROCESS` creation flags. No `setsid` equivalent needed — these flags detach from the parent console. |
| 3 | Handle leak to child | The sidecar must mark the ready HANDLE as non-inheritable before spawning the child, so the child doesn't hold the write end open. Use `SetHandleInformation` to clear `HANDLE_FLAG_INHERIT`. This is the Windows equivalent of `seal_ready_fd` (which is currently a no-op). |
| 4 | ReadyReader / ReadyWriter types | Both `File` (same as Unix). Windows `File` wraps an owned HANDLE internally. |

### Task 1: Add windows_sys pipe features

**Files:** `Cargo.toml`

Add `Win32_System_Pipes` to the windows-sys features:

```toml
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_System_Console",
    "Win32_System_IO",
    "Win32_System_JobObjects",
    "Win32_System_Pipes",
    "Win32_System_Threading",
    "Win32_Storage_FileSystem",
] }
```

**Verify:** `cargo check` on macOS.

**Commit:** `chore: add Win32_System_Pipes feature for anonymous and named pipes`

---

### Task 2: Implement `ready_channel`

**Files:** `src/platform/windows.rs`

Create an anonymous pipe. The write end must be inheritable (sidecar will inherit it). The read end stays non-inheritable (parent only).

```rust
fn ready_channel() -> io::Result<(File, File)> {
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let mut sa: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
    sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    sa.bInheritHandle = 1; // TRUE — handles are inheritable by default

    let mut read_handle = std::ptr::null_mut();
    let mut write_handle = std::ptr::null_mut();

    // SAFETY: sa is a valid SECURITY_ATTRIBUTES, pointers are valid out params.
    let ret = unsafe { CreatePipe(&mut read_handle, &mut write_handle, &sa, 0) };
    if ret == 0 {
        return Err(io::Error::last_os_error());
    }

    // Make read end non-inheritable — only the parent reads.
    set_handle_inheritable(read_handle, false)?;

    // SAFETY: both handles are valid from CreatePipe.
    let read_file = unsafe { File::from_raw_handle(read_handle as *mut _) };
    let write_file = unsafe { File::from_raw_handle(write_handle as *mut _) };

    Ok((read_file, write_file))
}
```

Add helper:

```rust
/// Set or clear the inheritable flag on a HANDLE.
fn set_handle_inheritable(
    handle: windows_sys::Win32::Foundation::HANDLE,
    inheritable: bool,
) -> io::Result<()> {
    use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
    use windows_sys::Win32::System::Threading::SetHandleInformation;

    let flags = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
    // SAFETY: handle is a valid HANDLE. SetHandleInformation is safe to call.
    let ret = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) };
    if ret == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
```

**Verify:** `cargo check` on macOS. `cargo check` on Windows.

**Commit:** `feat(windows): implement ready_channel with CreatePipe`

---

### Task 3: Implement `read_ready_signal` and `write_ready_signal`

**Files:** `src/platform/windows.rs`

These are identical to Unix — just read/write on a File:

```rust
fn read_ready_signal(mut reader: File) -> io::Result<String> {
    let mut buf = String::new();
    reader.read_to_string(&mut buf)?;
    if buf.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "sidecar died without signaling readiness",
        ));
    }
    Ok(buf)
}

fn write_ready_signal(mut writer: File, message: &str) -> io::Result<()> {
    writer.write_all(message.as_bytes())?;
    // writer is dropped here, closing the handle — reader sees EOF
    Ok(())
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement read/write_ready_signal`

---

### Task 4: Implement `ready_writer_from_env`

**Files:** `src/platform/windows.rs`

The sidecar reads the inherited HANDLE value from `TENDER_READY_HANDLE`:

```rust
fn ready_writer_from_env() -> io::Result<File> {
    let handle_str = std::env::var("TENDER_READY_HANDLE")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "TENDER_READY_HANDLE not set"))?;
    let handle: usize = handle_str.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "TENDER_READY_HANDLE is not a valid handle",
        )
    })?;
    // SAFETY: handle was inherited from the parent via CreatePipe with
    // bInheritHandle=TRUE. from_raw_handle takes ownership.
    Ok(unsafe { File::from_raw_handle(handle as *mut _) })
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement ready_writer_from_env`

---

### Task 5: Implement `seal_ready_fd`

**Files:** `src/platform/windows.rs`

Replace the no-op with `SetHandleInformation` to clear inheritability:

```rust
fn seal_ready_fd(writer: &File) -> io::Result<()> {
    // Mark the ready HANDLE as non-inheritable so the child process
    // doesn't hold the write end open (which would block the CLI's read).
    set_handle_inheritable(writer.as_raw_handle() as *mut _, false)
}
```

Note: this requires changing the function signature concern — `as_raw_handle()` returns `*mut c_void` on Windows, which needs to be cast to `HANDLE`. Check the actual type.

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement seal_ready_fd via SetHandleInformation`

---

### Task 6: Implement `spawn_sidecar`

**Files:** `src/platform/windows.rs`

```rust
fn spawn_sidecar(
    tender_bin: &Path,
    session_dir: &Path,
    ready_writer: &File,
) -> io::Result<u32> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
    };

    let handle_value = ready_writer.as_raw_handle() as usize;

    let mut cmd = Command::new(tender_bin);
    cmd.arg("_sidecar")
        .arg("--session-dir")
        .arg(session_dir)
        .env("TENDER_READY_HANDLE", handle_value.to_string());

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // DETACHED_PROCESS: no console allocation, detached from parent.
    // CREATE_NEW_PROCESS_GROUP: own process group for signal targeting.
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);

    let child = cmd.spawn()?;
    Ok(child.id())
}
```

**Key difference from Unix:** No `pre_exec` needed. Handle inheritance is controlled by `SECURITY_ATTRIBUTES.bInheritHandle` on the pipe (set in `ready_channel`). Detachment is via creation flags, not `setsid`.

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement spawn_sidecar with DETACHED_PROCESS`

---

### Task 7: Verify Slice 1 on Windows

Run on `rick-windows` via SSH.

**Expected to turn green (143 tests across 16 files):**

Core sidecar tests:
- `cargo test --test sidecar_ready` — 9 tests (start, status, list basics)
- `cargo test --test sidecar_child` — 10 tests (child lifecycle through sidecar)

CLI tests:
- `cargo test --test cli_start_idempotent` — 11 tests
- `cargo test --test cli_timeout` — 2 tests
- `cargo test --test cli_log` — 9 tests
- `cargo test --test cli_wait` — 6 tests
- `cargo test --test cli_replace` — 4 tests
- `cargo test --test cli_on_exit` — 5 tests
- `cargo test --test cli_watch` — 7 tests
- `cargo test --test cli_namespace` — 8 of 9 tests (1 needs push/Slice 2)
- `cargo test --test cli_prune` — 10 of 11 tests (1 Unix-only)
- `cargo test --test cli_wrap` — 11 of 12 tests (1 Unix-only SIGTERM test)

**Expected to still fail:**
- `cli_push` (7 tests) — needs Slice 2
- `cli_namespace::push_resolves_session_in_namespace` — needs Slice 2
- `cli_kill` (6 tests) + `cli_kill_forced` (2 tests) — needs Slice 3 (kill_orphan for sidecar-crashed cases) OR may partially work since kill_child is implemented
- `cli_reconcile` (3 tests) — Unix-only, stays gated

**Run full suite:** `cargo test` on Windows. Compare results with pre-slice baseline.

**Commit:** No code commit. Record results in plan progress section.

---

## Slice 2: Stdin Transport (Named Pipes)

**Unlocks:** `tender push` (8 tests).

### Design Decisions

| # | Decision | Choice |
|---|----------|--------|
| 0 | Transport mechanism | Named pipe at `\\.\pipe\tender-<session-hash>`. Path derived from session dir hash (Windows named pipes have a 256-char limit and can't use filesystem paths). |
| 1 | Server side | `CreateNamedPipeW` with `PIPE_ACCESS_INBOUND` (sidecar reads). Overlapped I/O not needed — the forwarding thread blocks on `ConnectNamedPipe`. |
| 2 | Client side | `CreateFileW` with `GENERIC_WRITE`. Maps `ERROR_PIPE_BUSY` to `ConnectionRefused` (same semantic as Unix ENXIO). |
| 3 | Multiple connections | After each client disconnects, call `DisconnectNamedPipe` + loop back to `ConnectNamedPipe`. Same outer-loop pattern as Unix FIFO. |
| 4 | StdinTransport type | Holds the named pipe server HANDLE + the pipe name (for cleanup). |

### Task 8: Implement `create_stdin_transport` and `StdinTransport`

**Files:** `src/platform/windows.rs`, `Cargo.toml`

Replace `StdinTransport` placeholder:

```rust
pub struct StdinTransport {
    pipe_handle: OwnedHandle,
    pipe_name: String,
}
```

Implement `create_stdin_transport`:

```rust
fn create_stdin_transport(session_dir: &Path) -> io::Result<StdinTransport> {
    let pipe_name = stdin_pipe_name(session_dir);
    let handle = create_named_pipe_server(&pipe_name)?;
    Ok(StdinTransport {
        pipe_handle: handle,
        pipe_name,
    })
}
```

Add helpers:

```rust
/// Derive a named pipe path from the session directory.
/// Uses a hash because Windows named pipes have a 256-char limit.
fn stdin_pipe_name(session_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(session_dir.as_os_str().as_encoded_bytes());
    let short = &hex::encode(hash)[..16]; // first 16 hex chars
    format!(r"\\.\pipe\tender-stdin-{short}")
}
```

Wait — tender doesn't have a `hex` dependency. Use the existing `sha2` + format manually:

```rust
fn stdin_pipe_name(session_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(session_dir.as_os_str().as_encoded_bytes());
    // First 8 bytes as hex = 16 hex chars, unique enough for local pipes.
    let hex: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!(r"\\.\pipe\tender-stdin-{hex}")
}
```

```rust
fn create_named_pipe_server(name: &str) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: wide_name is a valid null-terminated UTF-16 string.
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_INBOUND,         // server reads
            PIPE_TYPE_BYTE | PIPE_WAIT,  // byte mode, blocking
            1,                           // max instances
            0,                           // out buffer (not used for inbound)
            8192,                        // in buffer
            0,                           // default timeout
            std::ptr::null(),            // default security
        )
    };

    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut _) })
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement stdin transport with named pipes`

---

### Task 9: Implement `accept_stdin_connection`

**Files:** `src/platform/windows.rs`

```rust
fn accept_stdin_connection(
    transport: &StdinTransport,
    _session_dir: &Path,
) -> Option<Box<dyn io::Read + Send>> {
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;

    let handle = transport.pipe_handle.as_raw_handle() as *mut _;

    // SAFETY: handle is a valid named pipe from CreateNamedPipeW.
    // ConnectNamedPipe blocks until a client connects.
    let ret = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
    if ret == 0 {
        let err = io::Error::last_os_error();
        // ERROR_PIPE_CONNECTED means client connected before we called
        // ConnectNamedPipe — that's fine, proceed.
        if err.raw_os_error() != Some(
            windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED as i32
        ) {
            return None; // pipe broken or removed
        }
    }

    // Wrap the pipe handle in a reader. We need to clone/dup the handle
    // since OwnedHandle in StdinTransport still owns it for reuse.
    // Actually — on Windows named pipes, after ConnectNamedPipe, we read
    // from the same handle. We can't clone it easily, so we create a
    // wrapper that reads from the raw handle without owning it.
    Some(Box::new(PipeReader {
        handle: transport.pipe_handle.as_raw_handle() as *mut _,
    }))
}
```

This needs a `PipeReader` wrapper and a disconnect mechanism. After the reader is done (EOF), the forwarding loop in sidecar.rs will call `accept_stdin_connection` again, which needs `DisconnectNamedPipe` first.

Actually, the simpler approach: `accept_stdin_connection` disconnects the previous client (if any) before waiting for a new one. The reader wraps the raw handle.

```rust
/// Non-owning reader for a named pipe handle.
struct PipeReader {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: Windows HANDLEs can be sent between threads.
unsafe impl Send for PipeReader {}

impl io::Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use windows_sys::Win32::Storage::FileSystem::ReadFile;

        let mut bytes_read: u32 = 0;
        // SAFETY: handle is valid, buf is valid with correct length.
        let ret = unsafe {
            ReadFile(
                self.handle,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        if ret == 0 {
            let err = io::Error::last_os_error();
            // ERROR_BROKEN_PIPE = client disconnected = EOF
            if err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32)
            {
                return Ok(0); // EOF
            }
            return Err(err);
        }
        Ok(bytes_read as usize)
    }
}
```

And update `accept_stdin_connection` to disconnect previous client first:

```rust
fn accept_stdin_connection(
    transport: &StdinTransport,
    _session_dir: &Path,
) -> Option<Box<dyn io::Read + Send>> {
    use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe};

    let handle = transport.pipe_handle.as_raw_handle() as *mut _;

    // Disconnect previous client (no-op if none connected).
    unsafe { DisconnectNamedPipe(handle) };

    // Block until a new client connects.
    let ret = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
    if ret == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error()
            != Some(windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED as i32)
        {
            return None;
        }
    }

    Some(Box::new(PipeReader { handle }))
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement accept_stdin_connection with named pipe`

---

### Task 10: Implement `open_stdin_writer`

**Files:** `src/platform/windows.rs`

```rust
fn open_stdin_writer(session_dir: &Path) -> io::Result<File> {
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };
    use windows_sys::Win32::Storage::FileSystem::GENERIC_WRITE;

    let pipe_name = stdin_pipe_name(session_dir);
    let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();

    let handle = unsafe {
        CreateFileW(
            wide_name.as_ptr(),
            GENERIC_WRITE,
            0,                    // no sharing
            std::ptr::null(),     // default security
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(), // no template
        )
    };

    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        // ERROR_PIPE_BUSY = server exists but no ConnectNamedPipe pending
        // ERROR_FILE_NOT_FOUND = pipe doesn't exist yet
        if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
            || err.kind() == io::ErrorKind::NotFound
        {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, err));
        }
        return Err(err);
    }

    Ok(unsafe { File::from_raw_handle(handle as *mut _) })
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement open_stdin_writer for named pipe client`

---

### Task 11: Implement `remove_stdin_transport`

**Files:** `src/platform/windows.rs`

Named pipes are kernel objects — they're cleaned up when all handles are closed. No filesystem cleanup needed. But we should update the no-op to be explicit:

```rust
fn remove_stdin_transport(_session_dir: &Path) {
    // Named pipes are kernel objects cleaned up on handle close.
    // The StdinTransport's OwnedHandle drop handles this.
}
```

**Commit:** `chore(windows): document remove_stdin_transport no-op for named pipes`

---

### Task 12: Verify Slice 2 on Windows

Run on `rick-windows`:

**Expected to turn green (8 additional tests):**
- `cargo test --test cli_push` — 7 tests
- `cargo test --test cli_namespace -- push_resolves_session_in_namespace` — 1 test

**Run full suite:** `cargo test` on Windows.

---

## Slice 3: Orphan Kill

**Unlocks:** `tender kill` for crashed-sidecar recovery (11 tests).

### Design

On Unix, `kill_orphan` delegates to the same `kill_process` function used by `kill_child` — it sends signals to the process group by PID after verifying identity.

On Windows, orphan kill is harder because we don't have a Job Object handle (the sidecar that owned it crashed). We can only kill the individual process by PID, not its descendants.

### Task 13: Implement `kill_orphan`

**Files:** `src/platform/windows.rs`

```rust
fn kill_orphan(id: &ProcessIdentity, force: bool) -> io::Result<()> {
    // Without a Job Object handle (sidecar crashed), we can only kill the
    // process directly — no tree kill. This is a known degradation on Windows.
    match process_status(id) {
        ProcessStatus::Missing => return Ok(()),
        ProcessStatus::IdentityMismatch => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "PID was recycled — refusing to kill wrong process",
            ));
        }
        ProcessStatus::OsError(kind) => {
            return Err(io::Error::new(kind, "failed to probe process status"));
        }
        ProcessStatus::AliveVerified | ProcessStatus::Inaccessible => {}
    }

    if force {
        return terminate_process_by_pid(id.pid.get());
    }

    // Graceful: best-effort CTRL_BREAK, then wait, then force.
    send_ctrl_break(id.pid.get());

    if wait_for_process_exit(id.pid.get(), 5000) {
        return Ok(());
    }

    terminate_process_by_pid(id.pid.get())
}
```

Add helper:

```rust
/// Terminate a single process by PID. No tree kill — use only for orphans.
fn terminate_process_by_pid(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_TERMINATE,
    };

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        // Can't open — process likely already exited.
        return Ok(());
    }

    let ret = unsafe { TerminateProcess(handle, 1) };
    unsafe { CloseHandle(handle) };

    if ret == 0 {
        let err = io::Error::last_os_error();
        // Suppress "access denied" for already-terminated processes.
        if err.raw_os_error()
            != Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
        {
            return Err(err);
        }
    }
    Ok(())
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement kill_orphan with direct process termination`

---

### Task 14: Verify Slice 3 on Windows

Run on `rick-windows`:

**Expected to turn green (8 additional tests):**
- `cargo test --test cli_kill` — 6 tests
- `cargo test --test cli_kill_forced` — 2 tests

**Expected to stay Unix-only:**
- `cli_reconcile` (3 tests) — uses `libc::kill()` for sidecar signal injection

**Run full suite:** `cargo test` on Windows.

---

## Verification Summary

| Slice | Tests Unlocked | Key Commands |
|-------|---------------|-------------|
| 1 (sidecar spawn) | ~143 | start, status, list, watch, log, kill (live), wrap |
| 2 (stdin transport) | 8 | push |
| 3 (orphan kill) | 8 | kill (crashed sidecar) |

**Expected remaining Unix-only tests after all slices:**
- `cli_reconcile` (3) — uses `libc::kill()`
- `cli_prune::prune_delete_failure_reports_error_and_continues` (1) — uses `PermissionsExt`
- `cli_wrap::wrap_forwards_sigterm_and_writes_annotation` (1) — uses SIGTERM trap
- `session_fs` lock tests (5) — use `libc::flock()`

**Expected remaining failures to investigate separately:**
- `session_fs` meta read/write tests (4 pre-existing) — path separator or fsync behavior

---

## Files Summary

| File | Change |
|------|--------|
| `Cargo.toml` | Add `Win32_System_Pipes` feature |
| `src/platform/windows.rs` | Implement 8 remaining stubs + helpers |

## Not In Scope

- `STARTUPINFOEX` / `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` — simpler env-var handle passing is sufficient
- Windows CI on GitHub Actions — separate initiative
- `session_fs` pre-existing test failures — tracked separately
- `cli_reconcile` Windows equivalent — requires different signal injection mechanism
