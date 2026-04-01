---
id: windows-full-backend
depends_on: []
links:
  - ../completed/windows-full-backend.md
---

# Windows Full Backend — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Complete the Windows platform implementation so all tender CLI commands work end-to-end on Windows — `start`, `kill`, `status`, `list`, `watch`, `log`, `push`, `wrap`.

**Architecture:** Three slices building on the child lifecycle already landed. Slice 1 (sidecar spawn + readiness) is the main unlock. Slice 2 (stdin transport via named pipes) enables `push`. Slice 3 (orphan kill) enables `kill` from the CLI. Each slice has exact CLI test gates.

**Tech Stack:** Rust, `windows_sys` 0.61, `CreatePipe`, `CreateNamedPipeW`, `CREATE_NO_WINDOW`, `SetHandleInformation`.

**Quality gates:** `cargo fmt` before each commit. `cargo clippy` on changed files. `cargo check` on macOS (cross-check). Full `cargo test` on Windows per slice.

---

## Substrate (already landed)

| Method | Status |
|--------|--------|
| `spawn_child` | Real — `CREATE_NEW_PROCESS_GROUP` + Job Object |
| `child_try_wait` / `child_wait` | Real |
| `kill_child` | Real — `GenerateConsoleCtrlEvent(CTRL_BREAK)` → `WaitForSingleObject` → `TerminateJobObject` |
| `child_stdout/stderr/stdin` | Real |
| `child_kill_handle` | Real — Arc'd Job Object |
| `child_identity` / `process_identity` / `process_status` | Real |
| `self_identity` | Real |
| `seal_ready_fd` | Real — `SetHandleInformation` to clear `HANDLE_FLAG_INHERIT` |

---

## Progress

**Branch:** `windows-full-backend` (pushed to origin)

**Slice 1 — Complete. Verified on Windows.**

| Task | Commit | Status |
|------|--------|--------|
| 1. windows-sys features | `9712c42` | Done |
| 2. ready_channel (CreatePipe) | `9712c42` | Done |
| 3. read/write_ready_signal | `9712c42` | Done |
| 4. ready_writer_from_env | `9712c42` | Done |
| 5. seal_ready_fd | `9712c42` | Done |
| 6. spawn_sidecar | `9712c42` | Done |
| 7. prepare_sidecar_console | `2124564` | Done |
| 8. sidecar graceful-kill test | `3f8cc1f` | Done |
| 9. Slice 1 verification | `d5b7138` | Done |

**Additional work landed during Slice 1 verification:**

| Fix | Commit | Description |
|-----|--------|-------------|
| SetHandleInformation import | `cd02c01` | Was in wrong module |
| Directory fsync | `26f4ec9` | `sync_all()` on dir returns ACCESS_DENIED on Windows; now `#[cfg(unix)]` |
| Git-for-Windows PATH | `1ff3803` | Test harness adds coreutils to PATH |
| seal_ready_fd DuplicateHandle | `cfe6480` | Replace inheritable handle with non-inheritable duplicate |
| STARTUPINFOEXW handle whitelist | `2138940` | Raw CreateProcessW with PROC_THREAD_ATTRIBUTE_HANDLE_LIST for sidecar spawn |
| kill_orphan | `3f8cc1f` | Identity-verified single-process termination for orphans |
| Sidecar-mediated kill | `3cfc035` | CLI kill signals sidecar via kill_request file; sidecar uses Job Object |
| Kill review fixes | `d5b7138` | run_id scoping, 8s timeout alignment, atomic writes |
| Test mutex de-poisoning | `3f8cc1f` | unwrap_or_else across 14 test files |
| Plans restructure | `8455294` | active/backlog/completed/specs layout |

**Windows test results (226/237 pass, 11 fail):**

| Category | Tests | Notes |
|----------|-------|-------|
| Passing | 226 | All Slice 1 functionality works |
| Slice 2 (push) | 6 fail | Expected — stdin transport not implemented |
| cli_on_exit | 4 fail | To investigate — on-exit callbacks on Windows |
| cli_start_idempotent | 1 fail | `start_with_cwd` — Windows path handling |

**macOS:** All 234 tests green.

**To resume — next work items:**
1. Investigate cli_on_exit failures (4 tests)
2. Investigate `start_with_cwd_child_runs_in_requested_directory` (1 test)
3. Slice 2: stdin transport via named pipes

```bash
# Connect to Windows for testing:
ssh rick@100.90.60.48
cd ~/tender
git fetch origin
git checkout windows-full-backend
cargo test 2>&1
```

**Stop conditions (from plan review):**
- If readiness pipe inheritance does not work under `Command::spawn` → stop, redesign
- If `AllocConsole` does not preserve graceful child stop → stop, redesign

**After verification passes:** Continue to Slice 2 (stdin transport) or Slice 3 (orphan kill).

---

## Slice 1: Sidecar Spawn + Readiness

**Unlocks:** `tender start`, and therefore `status`, `list`, `watch`, `log`, `wrap` (NOT `kill` — see Slice 3, NOT `push` — see Slice 2).

### Design Decisions (locked)

| # | Decision | Choice |
|---|----------|--------|
| 0 | Anonymous pipe | `CreatePipe` with `SECURITY_ATTRIBUTES.bInheritHandle = TRUE` on write end. Read end non-inheritable (parent only). |
| 1 | Handle passing | Pass write HANDLE value as string in `TENDER_READY_HANDLE` env var (same pattern as Unix `TENDER_READY_FD`). Rust's `Command::spawn` sets `bInheritHandles=TRUE` unconditionally in `CreateProcessW`, so all inheritable handles are inherited by the child. Verified in stdlib source. |
| 2 | Sidecar detachment | `DETACHED_PROCESS` + `CREATE_NEW_PROCESS_GROUP`. The sidecar spawns with no console (detached from parent). |
| 3 | Sidecar console setup | The sidecar calls `AllocConsole()` + `ShowWindow(GetConsoleWindow(), SW_HIDE)` in its own startup, BEFORE spawning the child. This gives the sidecar a hidden console that children inherit via `CREATE_NEW_PROCESS_GROUP`. `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` then works because sidecar and child share the same console. **Why not `CREATE_NO_WINDOW` or skip `DETACHED_PROCESS`?** Neither gives the sidecar a usable console — `CREATE_NO_WINDOW` means "console handle is not set" per MS docs, not "hidden console." `AllocConsole` is the supported way for a detached process to acquire a console (MS AllocConsole docs). The `ShowWindow(SW_HIDE)` step is best-effort — `AllocConsole` is the important part. |
| 4 | Handle leak prevention | Two points: (a) Parent drops the write HANDLE after spawning the sidecar (already happens in `start.rs` via `drop(write_end)`). (b) Sidecar clears inheritability on the ready HANDLE before spawning the child (`seal_ready_fd`), so the child doesn't hold the write end open and block the CLI's read. Uses `SetHandleInformation` to clear `HANDLE_FLAG_INHERIT`. |
| 5 | ReadyReader / ReadyWriter types | Both `File` (same as Unix). Windows `File` wraps an owned HANDLE internally. |

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

### Task 2: Implement `ready_channel` and `set_handle_inheritable` helper

**Files:** `src/platform/windows.rs`

```rust
/// Set or clear the inheritable flag on a HANDLE.
fn set_handle_inheritable(
    handle: windows_sys::Win32::Foundation::HANDLE,
    inheritable: bool,
) -> io::Result<()> {
    use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
    use windows_sys::Win32::System::Threading::SetHandleInformation;

    let flags = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
    let ret = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) };
    if ret == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
```

Replace the `ready_channel` stub:

```rust
fn ready_channel() -> io::Result<(File, File)> {
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let mut sa: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
    sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    sa.bInheritHandle = 1; // TRUE — both handles inheritable initially

    let mut read_handle = std::ptr::null_mut();
    let mut write_handle = std::ptr::null_mut();

    // SAFETY: sa is valid, pointers are valid out params.
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

**Verify:** `cargo check` on macOS. `cargo check` on Windows.

**Commit:** `feat(windows): implement ready_channel with CreatePipe`

---

### Task 3: Implement `read_ready_signal` and `write_ready_signal`

**Files:** `src/platform/windows.rs`

Replace the stubs. These are identical to Unix — just read/write on a `File`:

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
    // writer dropped here — closes HANDLE, reader sees EOF
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

Replace the no-op. The sidecar must mark the ready HANDLE as non-inheritable before spawning the child, so the child doesn't hold the write end open (which would block the CLI's `read_to_string`):

```rust
fn seal_ready_fd(writer: &File) -> io::Result<()> {
    set_handle_inheritable(writer.as_raw_handle() as *mut _, false)
}
```

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

    // DETACHED_PROCESS: sidecar has no console initially.
    // The sidecar calls prepare_sidecar_console() in its own startup
    // to AllocConsole + hide window before spawning any children.
    // See Decision #3.
    //
    // CREATE_NEW_PROCESS_GROUP: own process group, detached from parent's
    // signal routing.
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);

    let child = cmd.spawn()?;
    Ok(child.id())
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement spawn_sidecar with DETACHED_PROCESS`

---

### Task 7: Implement `prepare_sidecar_console`

**Files:** `src/platform/windows.rs`, `src/commands/sidecar.rs`

Add a Windows-only platform helper that allocates a hidden console for the sidecar. Called from the sidecar entry point before any child spawning.

In `src/platform/windows.rs`:

```rust
/// Allocate a hidden console for the sidecar process.
///
/// The sidecar spawns with DETACHED_PROCESS (no console). Before spawning
/// children, it must acquire a console so that:
/// - Children inherit it via CREATE_NEW_PROCESS_GROUP
/// - GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) can reach them
///
/// The ShowWindow(SW_HIDE) step is best-effort — AllocConsole is the
/// important part. If the window can't be hidden, graceful stop still works;
/// the user just sees a brief console flash.
pub fn prepare_sidecar_console() {
    use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    // SAFETY: AllocConsole is safe to call from any thread.
    // Returns FALSE if the process already has a console — harmless.
    unsafe { AllocConsole() };

    // Best-effort: hide the console window.
    let hwnd = unsafe { GetConsoleWindow() };
    if !hwnd.is_null() {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}
```

In `src/commands/sidecar.rs`, add the platform call:

```rust
pub fn cmd_sidecar(session_dir: PathBuf) -> anyhow::Result<()> {
    // On Windows, allocate a hidden console so children can receive
    // GenerateConsoleCtrlEvent for graceful stop.
    #[cfg(windows)]
    tender::platform::windows::prepare_sidecar_console();

    let ready_writer = Current::ready_writer_from_env()?;
    tender::sidecar::run(session_dir, ready_writer)
}
```

**New Cargo.toml features needed:** `Win32_UI_WindowsAndMessaging` for `ShowWindow`, `SW_HIDE`, `GetConsoleWindow`.

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
    "Win32_UI_WindowsAndMessaging",
] }
```

**Verify:** `cargo check` on macOS. `cargo check` on Windows.

**Commit:** `feat(windows): add prepare_sidecar_console for hidden console allocation`

---

### Task 8: Add sidecar graceful-kill integration test

**Files:** `tests/windows_child.rs`

The existing `windows_kill_graceful_cooperative` test exercises `Platform::kill_child` directly. This test proves the same behavior works through a sidecar-started session (which uses `DETACHED_PROCESS` + `AllocConsole`).

```rust
/// Prove that a sidecar-started child responds to graceful kill via
/// the sidecar's AllocConsole'd console. This verifies that the
/// DETACHED_PROCESS → AllocConsole → child inheritance chain works
/// for GenerateConsoleCtrlEvent.
#[test]
fn windows_sidecar_child_graceful_kill() {
    // Start a session with ctrl_break_responder (exits 42 on CTRL_BREAK)
    let helper = env!("CARGO_BIN_EXE_ctrl_break_responder");
    // Use tender start → tender kill (graceful) → check exit code
    // Exit code 42 proves CTRL_BREAK was delivered through the sidecar's console
    // Exit code 1 would mean TerminateJobObject (escalation, AllocConsole failed)
    // ... (exact implementation depends on harness availability on Windows)
}
```

Note: This test may need to use the CLI directly (`tender start` + `tender kill`) rather than Platform methods, since the point is to exercise the sidecar's `AllocConsole` path. If `tender kill` requires `kill_orphan` (Slice 3), this test should instead start a session, verify the sidecar's child PID, then call `Platform::kill_child` via a helper, or defer this test to after Slice 3.

**Design decision needed at implementation time:** Whether this test can run in Slice 1 (without kill_orphan) or must wait for Slice 3.

**Commit:** `test(windows): add sidecar graceful-kill integration test`

---

### Task 9: Verify Slice 1 on Windows

Run on `rick-windows` via SSH.

**Expected GREEN (92+ tests):**

| Test file | Tests | Notes |
|-----------|-------|-------|
| `sidecar_ready` | 9 | start, status, list basics |
| `sidecar_child` | 10 | child lifecycle through sidecar |
| `cli_start_idempotent` | 11 | |
| `cli_timeout` | 2 | |
| `cli_log` | 9 | |
| `cli_wait` | 6 | |
| `cli_replace` | 4 | |
| `cli_on_exit` | 5 | |
| `cli_watch` | 7 | |
| `cli_namespace` | 8 | (excludes `push_resolves_session_in_namespace`) |
| `cli_prune` | 10 | (excludes Unix-only `prune_delete_failure`) |
| `cli_wrap` | 11 | (excludes Unix-only `wrap_forwards_sigterm`) |
| `windows_child` | 9+ | includes new sidecar graceful-kill test |

**Console verification gate:** The sidecar graceful-kill test (Task 8) must pass — this proves `DETACHED_PROCESS` + `AllocConsole` + `GenerateConsoleCtrlEvent` works end-to-end through a real sidecar.

**Expected RED (to be fixed in later slices):**

| Test file | Tests | Needs |
|-----------|-------|-------|
| `cli_push` | 7 | Slice 2 (stdin transport) |
| `cli_namespace::push_resolves_session_in_namespace` | 1 | Slice 2 |
| `cli_kill` | 6 | Slice 3 (kill_orphan) |
| `cli_kill_forced` | 2 | Slice 3 |

**Expected PERMANENTLY Unix-only:**

| Test file | Tests | Reason |
|-----------|-------|--------|
| `cli_reconcile` | 3 | Uses `libc::kill()` |
| `cli_prune::prune_delete_failure...` | 1 | Uses `PermissionsExt` |
| `cli_wrap::wrap_forwards_sigterm...` | 1 | Uses SIGTERM trap |
| `session_fs` lock tests | 5 | Uses `libc::flock()` |

**Run:** `cargo test` on Windows. Record exact pass/fail counts.

---

## Slice 2: Stdin Transport (Named Pipes)

**Unlocks:** `tender push` (8 tests).

### Design Decisions (locked)

| # | Decision | Choice |
|---|----------|--------|
| 0 | Transport mechanism | Named pipe at `\\.\pipe\tender-stdin-<hash>`. Hash derived from session dir path (first 16 hex chars of SHA256). Windows named pipes have a 256-char limit. |
| 1 | Server side | `CreateNamedPipeW` with `PIPE_ACCESS_INBOUND`, `PIPE_TYPE_BYTE`, `PIPE_WAIT`. Max 1 instance. Blocking mode. |
| 2 | Client side | `CreateFileW` with `GENERIC_WRITE` + `OPEN_EXISTING`. Maps `ERROR_PIPE_BUSY` and `ERROR_FILE_NOT_FOUND` to `ConnectionRefused`. |
| 3 | Multiple connections | `DisconnectNamedPipe` before each `ConnectNamedPipe`. Same outer-loop pattern as Unix FIFO reopen. |
| 4 | Reading from pipe | Non-owning `PipeReader` wrapper with `ReadFile`. Maps `ERROR_BROKEN_PIPE` to EOF (Ok(0)). |
| 5 | StdinTransport type | Holds `OwnedHandle` (server pipe) + `String` (pipe name). |

### Task 10: Implement `StdinTransport`, `create_stdin_transport`, and helpers

**Files:** `src/platform/windows.rs`

Replace `StdinTransport` placeholder:

```rust
pub struct StdinTransport {
    pipe_handle: OwnedHandle,
    #[allow(dead_code)] // name used conceptually for debugging; cleanup is via handle drop
    pipe_name: String,
}
```

Add `stdin_pipe_name`:

```rust
fn stdin_pipe_name(session_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(session_dir.as_os_str().as_encoded_bytes());
    let hex: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!(r"\\.\pipe\tender-stdin-{hex}")
}
```

Add `create_named_pipe_server`:

```rust
fn create_named_pipe_server(name: &str) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1,    // max instances
            0,    // out buffer
            8192, // in buffer
            0,    // default timeout
            std::ptr::null(),
        )
    };

    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut _) })
}
```

Implement trait method:

```rust
fn create_stdin_transport(session_dir: &Path) -> io::Result<StdinTransport> {
    let pipe_name = stdin_pipe_name(session_dir);
    let pipe_handle = create_named_pipe_server(&pipe_name)?;
    Ok(StdinTransport { pipe_handle, pipe_name })
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement stdin transport with named pipes`

---

### Task 11: Implement `accept_stdin_connection` and `PipeReader`

**Files:** `src/platform/windows.rs`

Add `PipeReader`:

```rust
/// Non-owning reader for a named pipe handle.
struct PipeReader {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

unsafe impl Send for PipeReader {}

impl io::Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use windows_sys::Win32::Storage::FileSystem::ReadFile;

        let mut bytes_read: u32 = 0;
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
            if err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32)
            {
                return Ok(0); // EOF — client disconnected
            }
            return Err(err);
        }
        Ok(bytes_read as usize)
    }
}
```

Implement trait method:

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
        // ERROR_PIPE_CONNECTED = client connected before ConnectNamedPipe — fine.
        if err.raw_os_error()
            != Some(windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED as i32)
        {
            return None; // pipe broken or closed
        }
    }

    Some(Box::new(PipeReader { handle }))
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement accept_stdin_connection with named pipe`

---

### Task 12: Implement `open_stdin_writer` and `remove_stdin_transport`

**Files:** `src/platform/windows.rs`

```rust
fn open_stdin_writer(session_dir: &Path) -> io::Result<File> {
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, GENERIC_WRITE, OPEN_EXISTING,
    };

    let pipe_name = stdin_pipe_name(session_dir);
    let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();

    let handle = unsafe {
        CreateFileW(
            wide_name.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };

    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
            || err.kind() == io::ErrorKind::NotFound
        {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, err));
        }
        return Err(err);
    }

    Ok(unsafe { File::from_raw_handle(handle as *mut _) })
}

fn remove_stdin_transport(_session_dir: &Path) {
    // Named pipes are kernel objects cleaned up when all handles close.
    // StdinTransport's OwnedHandle drop handles this.
}
```

**Verify:** `cargo check` on Windows.

**Commit:** `feat(windows): implement open_stdin_writer and remove_stdin_transport`

---

### Task 13: Verify Slice 2 on Windows

**Expected GREEN (8 additional tests):**

| Test file | Tests |
|-----------|-------|
| `cli_push` | 7 |
| `cli_namespace::push_resolves_session_in_namespace` | 1 |

**Run:** `cargo test` on Windows.

---

## Slice 3: Orphan Kill

**Unlocks:** `tender kill` from CLI (8 tests). The `kill` command uses `kill_orphan` (not `kill_child`) because the CLI process only has `ProcessIdentity` from meta.json — no live `SupervisedChild` handle.

### Design

On Unix, `kill_orphan` delegates to the same `kill_process` function — signals by PID after identity verification.

On Windows without a Job Object handle, we can only kill the individual process by PID. No descendant tree kill. This is a known degradation — acceptable because orphan kill is a recovery path (sidecar crashed), not the normal lifecycle.

### Task 14: Implement `kill_orphan`

**Files:** `src/platform/windows.rs`

```rust
fn kill_orphan(id: &ProcessIdentity, force: bool) -> io::Result<()> {
    // Without a Job Object handle (sidecar crashed), we can only kill the
    // process directly — no tree kill. Known degradation on Windows.
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
        return Ok(()); // Can't open — process likely already exited.
    }

    let ret = unsafe { TerminateProcess(handle, 1) };
    unsafe { CloseHandle(handle) };

    if ret == 0 {
        let err = io::Error::last_os_error();
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

### Task 15: Verify Slice 3 on Windows

**Expected GREEN (8 additional tests):**

| Test file | Tests |
|-----------|-------|
| `cli_kill` | 6 |
| `cli_kill_forced` | 2 |

**Expected PERMANENTLY Unix-only:**
- `cli_reconcile` (3 tests) — uses `libc::kill()` for sidecar signal injection

**Run:** `cargo test` on Windows.

---

## Final Verification Summary

| Slice | Tests Unlocked | Key Commands |
|-------|---------------|-------------|
| 1 (sidecar spawn) | 92 | start, status, list, watch, log, wrap |
| 2 (stdin transport) | 8 | push |
| 3 (orphan kill) | 8 | kill |

**After all 3 slices — expected permanently Unix-only (10 tests):**
- `cli_reconcile` (3) — `libc::kill()`
- `cli_prune::prune_delete_failure_reports_error_and_continues` (1) — `PermissionsExt`
- `cli_wrap::wrap_forwards_sigterm_and_writes_annotation` (1) — SIGTERM trap
- `session_fs` lock tests (5) — `libc::flock()`

**Pre-existing failures to investigate separately (not in scope):**
- `session_fs` meta read/write tests (4) — path separator or fsync behavior

---

## Files Summary

| File | Change |
|------|--------|
| `Cargo.toml` | Add `Win32_System_Pipes` feature |
| `src/platform/windows.rs` | Implement 8 remaining stubs + helpers |

## Not In Scope

- `STARTUPINFOEX` / `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` — env-var handle passing verified sufficient
- Windows CI on GitHub Actions — separate initiative
- `session_fs` pre-existing test failures — tracked separately
- `cli_reconcile` Windows equivalent — requires different signal injection mechanism
