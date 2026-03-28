# Phase 2A: Platform Trait Seam + Windows Backend

## Design Principle

**Design from Windows, not Unix.** The trait surface is derived from
Windows semantics so Unix adapts to a clean contract instead of Windows
trying to squeeze into Unix-shaped abstractions.

---

## 1. Windows Process Model — Key Semantics

### Spawn
- `CreateProcessW` is atomic (no fork+exec gap, no `pre_exec`)
- `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` ≈ `setsid()`
- `CREATE_SUSPENDED` → assign to Job Object → `ResumeThread`
- Handle inheritance via `STARTUPINFOEX` + `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
  (no CLOEXEC dance — atomic, race-free)

### Process Tree Kill
- **Job Objects** capture the entire descendant tree automatically
- `TerminateJobObject(hJob, 1)` = force kill entire tree (better than `kill(-pgid)`)
- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` = crash-safe cleanup: if sidecar
  dies, OS kills the tree when the handle is closed
- Nested jobs work on Windows 8+ (not a concern for us)

### Process Identity
- `HANDLE` from `CreateProcess` is inherently PID-reuse-safe (kernel
  object stays alive until handle closed, even after process exits)
- For orphan recovery (no handle): `OpenProcess(pid)` + `GetProcessTimes`
  to check `CreationTime` — maps directly to our `ProcessIdentity`
- `CreationTime` is `FILETIME` (100ns since 1601-01-01) — convert to our
  `start_time_ns` by subtracting epoch offset

### Graceful Kill
- **No SIGTERM equivalent for detached processes**
- `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` only works for console processes
- Our sidecar uses `DETACHED_PROCESS` → Ctrl events don't reach it
- Options: named event object, or "please exit" message over stdin pipe
- Decision: **use a named event for graceful stop, TerminateJobObject for force**

### stdin (Named Pipes)
- `CreateNamedPipeW("\\.\pipe\tender-<session>-stdin", PIPE_ACCESS_INBOUND, ...)`
- Server (sidecar) calls `ConnectNamedPipe` to wait for client
- Client (push) calls `CreateFile` on pipe path — fails immediately if no server
- After client disconnects: `DisconnectNamedPipe` → `ConnectNamedPipe` for next client
- Key difference: Windows named pipes live in kernel namespace, not filesystem

### Readiness Channel
- Anonymous pipe via `CreatePipe` — direct equivalent of our Unix pattern
- Write end inherited by sidecar via `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
- Sidecar writes `OK:<json>`, closes handle. Parent reads until EOF.

---

## 2. Platform Trait

The central insight: **child supervision is the hardest platform seam,**
not sidecar spawn. On Unix, `spawn_child` calls `pre_exec(setpgid)` and
the sidecar holds a `std::process::Child`. On Windows, `spawn_child`
calls `CreateProcessW` with `CREATE_SUSPENDED`, creates a Job Object,
assigns the process, resumes it, and holds a `(HANDLE, HANDLE, HANDLE)`
tuple (process, job, graceful-stop event). The kill path then uses these
handles directly.

The trait must therefore own the **supervised child context** — an opaque
type that carries whatever backend-specific state the kill/wait/stdin
paths need.

```rust
/// Platform-specific process supervision operations.
///
/// The key abstraction is `SupervisedChild`: an opaque bundle of
/// backend state for a child process under supervision. On Unix this
/// wraps std::process::Child + ProcessIdentity. On Windows this wraps
/// a process HANDLE, Job Object HANDLE, graceful-stop event HANDLE,
/// and stdout/stderr pipe HANDLEs.
///
/// Callers never see raw HANDLEs, PIDs, or fds — only this type.
pub trait Platform {
    /// Opaque supervised-child state. Dropped when supervision ends.
    type SupervisedChild;
    type StdinTransport;
    type ReadyReader;
    type ReadyWriter;

    // --- Sidecar spawn (CLI side) ---

    /// Spawn the sidecar as a detached process.
    /// Returns the sidecar PID (for meta.json).
    fn spawn_sidecar(
        tender_bin: &Path,
        session_dir: &Path,
        ready_writer: &Self::ReadyWriter,
    ) -> io::Result<u32>;

    // --- Readiness channel ---

    /// Create a readiness channel (anonymous pipe).
    fn ready_channel() -> io::Result<(Self::ReadyReader, Self::ReadyWriter)>;

    /// Block until the sidecar writes a readiness message.
    fn read_ready_signal(reader: Self::ReadyReader) -> io::Result<String>;

    /// Write a readiness message from the sidecar side.
    fn write_ready_signal(writer: Self::ReadyWriter, message: &str) -> io::Result<()>;

    // --- Child spawn (sidecar side) ---
    //
    // SupervisedChild lifecycle:
    //
    //   spawn_child()        → creates SupervisedChild (owns process + I/O handles)
    //   child_identity()     → borrows, reads PID + start time (callable any time)
    //   child_stdout/stderr  → take I/O streams out (Option → None after first call)
    //   child_stdin          → take stdin handle out (same take-once semantics)
    //   kill_child()         → borrows; safe to call after streams are taken
    //   child_wait()         → borrows mutably; blocks until exit
    //   drop(child)          → closes remaining handles/descriptors
    //
    // Thread safety for the sidecar supervision loop:
    //
    //   1. Main thread owns the SupervisedChild
    //   2. Before child_wait, take stdout/stderr/stdin via the take methods
    //      and move them to capture/forwarding threads
    //   3. Timeout thread gets a clone of child_identity (Copy) and calls
    //      kill_child through a shared reference — kill_child takes &self
    //      and is safe to call concurrently with child_wait
    //   4. child_wait blocks the main thread until exit
    //   5. After child_wait returns, capture threads finish naturally
    //      (their streams hit EOF)
    //   6. Main thread drops SupervisedChild (closes job handle, etc.)
    //
    // The take-once pattern for I/O streams means ownership transfers
    // cleanly to capture/forwarding threads without shared state.
    // kill_child takes &self (not &mut self) because the kill path only
    // needs the process/job handles, not the I/O streams.

    /// Spawn a child process under supervision.
    ///
    /// Sets up platform-specific process grouping/containment:
    /// - Unix: setpgid(0,0) to create new process group
    /// - Windows: CREATE_SUSPENDED → Job Object → ResumeThread
    ///
    /// Returns an opaque SupervisedChild that owns all backend state.
    fn spawn_child(
        argv: &[String],
        stdin_piped: bool,
    ) -> io::Result<Self::SupervisedChild>;

    /// Get the ProcessIdentity of the supervised child.
    /// Cheap, callable any time — borrows only.
    fn child_identity(child: &Self::SupervisedChild) -> io::Result<ProcessIdentity>;

    /// Wait for the child to exit. Blocks until the process terminates.
    /// Does NOT consume the child — handles stay open for cleanup.
    fn child_wait(child: &mut Self::SupervisedChild) -> io::Result<std::process::ExitStatus>;

    /// Take the child's stdout stream for capture.
    /// Returns None if already taken. Moves ownership to the caller
    /// (typically a capture thread).
    fn child_stdout(child: &mut Self::SupervisedChild) -> Option<Box<dyn io::Read + Send>>;

    /// Take the child's stderr stream for capture.
    /// Same take-once semantics as child_stdout.
    fn child_stderr(child: &mut Self::SupervisedChild) -> Option<Box<dyn io::Read + Send>>;

    /// Take the child's stdin for forwarding (if stdin_piped was true).
    /// Same take-once semantics.
    fn child_stdin(child: &mut Self::SupervisedChild) -> Option<Box<dyn io::Write + Send>>;

    // --- Kill ---

    /// Kill a supervised child (sidecar has the live context).
    ///
    /// Takes `&self` — safe to call concurrently with child_wait from
    /// a timeout thread. Does NOT require stdout/stderr/stdin to still
    /// be present (they may have been taken by capture threads).
    ///
    /// - Unix: kill(-pgid, signal) with identity verification
    /// - Windows: TerminateJobObject (force) or SetEvent (graceful)
    fn kill_child(child: &Self::SupervisedChild, force: bool) -> io::Result<()>;

    /// Kill an orphaned process by persisted identity (no live handle).
    ///
    /// Used when the sidecar has crashed and we only have ProcessIdentity
    /// from meta.json or the child_pid breadcrumb.
    ///
    /// `force` parameter is accepted but **graceful is best-effort only:**
    /// - Unix force=false: SIGTERM → wait → SIGKILL (direct PID, no group)
    /// - Unix force=true: SIGKILL (direct PID)
    /// - Windows force=false: degrades to force (no stop event available)
    /// - Windows force=true: OpenProcess → TerminateProcess (single process,
    ///   no tree kill — descendants may survive)
    ///
    /// This is honestly weaker than kill_child. The degradation is
    /// documented, not hidden.
    fn kill_orphan(id: &ProcessIdentity, force: bool) -> io::Result<()>;

    // --- Process identity (orphan recovery) ---

    /// Get the identity of any process by PID.
    fn process_identity(pid: u32) -> io::Result<ProcessIdentity>;

    /// Probe whether a process is alive and matches the given identity.
    fn process_status(id: &ProcessIdentity) -> ProcessStatus;

    // --- stdin transport ---

    /// Create the stdin transport (sidecar side).
    /// Unix: mkfifo. Windows: CreateNamedPipe.
    fn create_stdin_transport(session_dir: &Path) -> io::Result<Self::StdinTransport>;

    /// Open the stdin transport for writing (push command side).
    /// Returns immediately if no reader (ENXIO / ERROR_FILE_NOT_FOUND).
    fn open_stdin_writer(session_dir: &Path) -> io::Result<File>;

    /// Remove the stdin transport on cleanup.
    fn remove_stdin_transport(session_dir: &Path);

    // --- Ready fd inheritance (sidecar side) ---

    /// Prevent the ready channel from leaking to the child process.
    /// Unix: set CLOEXEC on the ready fd.
    /// Windows: no-op (HANDLE_LIST already controls inheritance).
    fn seal_ready_fd(writer: &Self::ReadyWriter) -> io::Result<()>;
}
```

### Why two kill methods

The sidecar path (`kill_child`) has the live `SupervisedChild` context —
on Windows this includes the Job Object handle and graceful-stop event.
This is the strong kill: it can terminate the entire descendant tree and
attempt graceful shutdown.

The CLI path (`kill_orphan`) only has a `ProcessIdentity` from
meta.json. The sidecar may be alive (in which case `kill_child` runs
inside it via the child process signal), or the sidecar may have
crashed (orphan case). `kill_orphan` is inherently weaker:

| | `kill_child` (sidecar has context) | `kill_orphan` (CLI, no context) |
|---|---|---|
| **Unix graceful** | `kill(-pgid, SIGTERM)` → full group | `kill(pid, SIGTERM)` → direct only |
| **Unix force** | `kill(-pgid, SIGKILL)` → full group | `kill(pid, SIGKILL)` → direct only |
| **Windows graceful** | `SetEvent(stop_event)` → wait → `TerminateJobObject` | **degrades to force** (no event handle) |
| **Windows force** | `TerminateJobObject` → full tree | `OpenProcess` + `TerminateProcess` → single process |

Orphan graceful on Windows degrades silently to force kill. This is the
right trade-off: the orphan case means the sidecar crashed, so the
child's graceful-stop event handle is gone. Forcing is better than
returning an error that leaves the orphan running.

### SupervisedChild ownership summary

```
                    spawn_child()
                         │
                         ▼
               ┌─────────────────────┐
               │  SupervisedChild    │
               │                     │
               │  process handle ────┼── kept for kill_child, child_wait
               │  job handle (Win) ──┼── kept for kill_child (tree kill)
               │  stop event (Win) ──┼── kept for kill_child (graceful)
               │  identity ──────────┼── copied out by child_identity
               │  stdout ────────────┼── taken by child_stdout → capture thread
               │  stderr ────────────┼── taken by child_stderr → capture thread
               │  stdin  ────────────┼── taken by child_stdin  → forwarding thread
               └─────────────────────┘
                         │
          ┌──────────────┼───────────────┐
          ▼              ▼               ▼
   capture threads   child_wait()   timeout thread
   (own stdout/err)  (blocks main)  (calls kill_child
                                     via &child)
          │              │               │
          ▼              ▼               ▼
       streams EOF    exit status    cancel flag set
                         │
                         ▼
                    drop(child)
                  closes handles
```

Key rules:
1. **I/O streams are take-once** — `Option::take` semantics, moved to threads
2. **`kill_child` takes `&self`** — concurrent with `child_wait`, no mutex needed
3. **`child_wait` takes `&mut self`** — only the main thread waits
4. **Drop closes handles** — if sidecar crashes, OS cleans up via `KILL_ON_JOB_CLOSE` (Windows) or orphan breadcrumb (Unix)

### What stays backend-specific (NOT in the trait)

| Operation | Unix | Windows | Why not shared |
|-----------|------|---------|----------------|
| Process group setup | `setpgid(0,0)` in `pre_exec` | Job Object assignment | Completely different APIs |
| Process detach | `setsid()` in `pre_exec` | `DETACHED_PROCESS` flag | Unix needs post-fork hook |
| fd/handle inheritance | `fcntl(F_SETFD)` | `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` | Fundamentally different models |
| Graceful signal | `SIGTERM` via `kill(2)` | Named event (`SetEvent`) | No common primitive |
| Process start time | `/proc/pid/stat` or `proc_pidinfo` | `GetProcessTimes` | Platform-specific APIs |
| Job Object lifecycle | N/A | `CreateJobObject` + handle | Windows-only concept |
| Crash-safe cleanup | Orphan breadcrumb + reconciliation | `KILL_ON_JOB_CLOSE` (automatic) | Unix has no kernel equivalent |

### What's shared (in `src/` not behind the trait)

- `ProcessIdentity { pid, start_time_ns }` — same struct, both platforms
- `ProcessStatus` enum — same variants
- State machine (`RunStatus`, transitions)
- Meta persistence (meta.json, atomic writes)
- Log capture logic (read child stdout/stderr, write output.log) —
  the streams come from `child_stdout`/`child_stderr`, but the
  timestamp-and-tag formatting and file writes are shared
- All CLI command dispatch
- Timeout thread logic (uses `kill_child` instead of raw signals)

---

## 3. Semantic Mismatches

| Feature | Unix | Windows | Resolution |
|---------|------|---------|------------|
| **Child spawn** | `Command::new` + `pre_exec(setpgid)` | `CreateProcessW` + `CREATE_SUSPENDED` + Job Object + `ResumeThread` | Behind `spawn_child` — the heaviest method |
| **Graceful kill** | `SIGTERM` → child handler runs | Named event → child polls/waits | `kill_child(force=false)` — each backend does its own graceful |
| **Tree kill** | `kill(-pgid)` — only direct group members | `TerminateJobObject` — entire descendant tree | Job Object is strictly better; trait abstracts both |
| **Crash-safe cleanup** | Orphan breadcrumb + reconciliation | `KILL_ON_JOB_CLOSE` — OS auto-kills | Both paths needed; Windows gets it for free |
| **stdin transport** | Filesystem FIFO (`mkfifo`) | Kernel-namespace named pipe (`CreateNamedPipeW`) | Different `create_stdin_transport` impls |
| **Readiness channel** | `pipe()` + fd inheritance | `CreatePipe` + handle inheritance | Same pattern, different APIs |
| **Ready fd sealing** | `fcntl(CLOEXEC)` after sidecar starts | No-op (handle list is precise) | `seal_ready_fd` — trivial on both sides |
| **Process liveness** | PID recycled immediately after exit | HANDLE prevents recycling while held | `SupervisedChild` holds handle on Windows |

---

## 4. Implementation Slices

### 2A.1: Trait definition + Unix adaptation (no behavior changes)

1. Define `Platform` trait in `src/platform/mod.rs`
2. Create `src/platform/unix.rs::UnixPlatform` implementing the trait
   - `SupervisedChild` wraps `std::process::Child` + `ProcessIdentity`
   - `kill_child` uses the existing `kill_process` (SIGTERM → SIGKILL via group)
   - `kill_orphan` uses the existing `kill_process` (direct PID, no group)
   - `spawn_child` absorbs the current `spawn_child` + `setpgid` from sidecar.rs
   - `child_identity` uses existing `process_identity(child.id())`
   - `create_stdin_transport` wraps `mkfifo`
   - `open_stdin_writer` wraps `open_fifo_write_nonblock`
   - `seal_ready_fd` wraps the `fcntl(CLOEXEC)` call from sidecar.rs
3. Refactor `src/sidecar.rs` to call trait methods:
   - `Platform::spawn_child` instead of local `spawn_child`
   - `Platform::child_identity` instead of `platform::process_identity`
   - `Platform::kill_child` from timeout thread
   - `Platform::child_stdout`/`child_stderr` for capture
   - `Platform::child_stdin` for forwarding
   - `Platform::seal_ready_fd` instead of inline `fcntl`
4. Refactor `src/commands/` to call `Platform::kill_orphan` instead of
   `platform::kill_process` for the CLI kill/replace paths
5. All 177 tests (at time of writing) pass with zero behavior change

**Compile-time dispatch** (not dynamic dispatch):
```rust
// src/platform/mod.rs
#[cfg(unix)]
pub mod unix;
#[cfg(unix)]
pub type Current = unix::UnixPlatform;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub type Current = windows::WindowsPlatform;

// Callers use: platform::Current::spawn_child(...)
```

### 2A.2: Windows module skeleton + process identity

1. `src/platform/windows.rs` with `WindowsPlatform` struct
2. Implement `process_identity` via `OpenProcess` + `GetProcessTimes`
3. Implement `process_status` via identity check
4. All other trait methods return `Err(io::Error::new(Unsupported, ...))`
   — **not** `unimplemented!()` panics
5. Cross-compile check: `cargo check --target x86_64-pc-windows-msvc`

### 2A.3: Windows child spawn + Job Object kill

1. `SupervisedChild` struct: `{ process_handle, job_handle, stop_event, pid }`
2. `spawn_child`: `CreateProcessW(CREATE_SUSPENDED)` → `CreateJobObject` →
   `AssignProcessToJobObject` → `ResumeThread`
3. `kill_child(force=true)`: `TerminateJobObject`
4. `kill_child(force=false)`: `SetEvent(stop_event)` → wait → `TerminateJobObject`
5. `kill_orphan`: `OpenProcess` + `TerminateProcess` (no tree kill — documented limitation)
6. `child_wait`: `WaitForSingleObject(process_handle, INFINITE)`

### 2A.4: Windows sidecar spawn + readiness

1. `spawn_sidecar`: `CreateProcessW(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)`
   with `STARTUPINFOEX` handle list for pipe inheritance
2. `ready_channel`: `CreatePipe` with `SECURITY_ATTRIBUTES.bInheritHandle`
3. `read_ready_signal` / `write_ready_signal`: `ReadFile` / `WriteFile`
4. `seal_ready_fd`: no-op (handle list already precise)

### 2A.5: Windows stdin + integration tests

1. `create_stdin_transport` via `CreateNamedPipeW`
2. `open_stdin_writer` via `CreateFile` on pipe path
3. Port integration tests to work on both platforms
4. CI: add Windows test target

---

## 5. Success Criteria

- [ ] One `Platform` trait covering sidecar spawn, child spawn/supervision,
      kill (both live and orphan), identity, stdin, and readiness
- [ ] `SupervisedChild` carries all backend state needed for kill/wait
- [ ] Every Phase 1 feature has an explicit Windows mapping
- [ ] Unix passes all 177 tests (at time of writing) with trait in place (no behavior change)
- [ ] Windows compiles with `Err(Unsupported)` stubs (no panics)
- [ ] Semantic mismatches documented with resolution
- [ ] No Unix concepts in the trait (mkfifo, setsid, -pid, CLOEXEC)
- [ ] No Windows concepts in the trait (HANDLE, Job Object, FILETIME)
- [ ] `kill_child` vs `kill_orphan` distinction is clear and honest
      about the strength difference

---

## 6. Dependencies

| Crate | Purpose | When |
|-------|---------|------|
| `windows-sys` | Win32 API bindings (zero-cost, no COM) | 2A.2 |
| `rustix` (existing) | Unix backend stays as-is | Already added |

`windows-sys` over `windows` crate: lighter, no COM wrapper overhead,
just raw FFI bindings. Same relationship as `libc` to higher-level crates.
