# PTY Session Mode Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add PTY-backed sessions so agents can drive terminal-sensitive programs and humans can attach/detach for live control.

**Architecture:** The sidecar gains a second execution lane: when `io_mode: "pty"`, it allocates a PTY pair instead of pipes, captures merged transcript, forwards FIFO push input to the PTY master, and listens on a Unix domain socket for human attach. A control state model (AgentControl/HumanControl/Detached) governs who can send input.

**Key design decisions:**

- **PTY fits the existing Platform trait** — `spawn_child_pty` returns a `SupervisedChild` where `child_stdout()` returns the PTY master read half and `child_stdin()` returns the PTY master write half. The sidecar does not reach inside `SupervisedChild` — it uses the same trait accessors as pipe mode.
- **Sidecar owns a `PtyBroker`** — The PTY master write side is shared between FIFO forwarding (push), attach relay (human input), and resize. The sidecar wraps it in `Arc<Mutex<File>>` and owns the broker. Transcript capture reads from the read half (taken via `child_stdout()`). This avoids the problem of a single `Option<OwnedFd>` being taken multiple times.
- **No agent lease in slice one** — Human detach always returns to `AgentControl`. The `Detached` state only applies at startup if the session has no push channel (`--stdin` not set). Lease/heartbeat is deferred.

**Tech Stack:** Rust, `libc` for `openpty`/`setsid`/`ioctl`, `rustix` with `termios` feature for terminal control, Unix domain sockets via `std::os::unix::net`.

---

### Task 1: Add `IoMode` enum and `io_mode` field to LaunchSpec

**Files:**
- Modify: `src/model/spec.rs`

**Step 1: Write the failing test**

Add to `tests/model_ids.rs` (or create `tests/model_spec.rs`):

```rust
#[test]
fn launch_spec_io_mode_defaults_to_pipe() {
    let spec = tender::model::spec::LaunchSpec::new(vec!["echo".into()]).unwrap();
    assert_eq!(spec.io_mode, tender::model::spec::IoMode::Pipe);
}

#[test]
fn launch_spec_pty_mode_serializes() {
    let mut spec = tender::model::spec::LaunchSpec::new(vec!["bash".into()]).unwrap();
    spec.io_mode = tender::model::spec::IoMode::Pty;
    let json = serde_json::to_string(&spec).unwrap();
    assert!(json.contains("\"io_mode\":\"Pty\""), "json: {json}");
}

#[test]
fn launch_spec_without_io_mode_deserializes_as_pipe() {
    // Backward compat: old launch_spec.json files won't have io_mode
    let json = r#"{"argv":["echo"],"stdin_mode":"None"}"#;
    let spec: tender::model::spec::LaunchSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.io_mode, tender::model::spec::IoMode::Pipe);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test model_spec`
Expected: FAIL — `IoMode` doesn't exist.

**Step 3: Implement IoMode and add to LaunchSpec**

In `src/model/spec.rs`, add after `StdinMode`:

```rust
/// How the session's child I/O is wired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IoMode {
    /// Pipes: stdout/stderr captured separately, stdin via FIFO if enabled.
    #[default]
    Pipe,
    /// Pseudo-terminal: merged I/O, interactive terminal.
    Pty,
}
```

Add field to `LaunchSpec` struct:

```rust
    pub stdin_mode: StdinMode,
    #[serde(default)]
    pub io_mode: IoMode,
```

Update `LaunchSpec::new()` to initialize:

```rust
    io_mode: IoMode::Pipe,
```

Update the `Raw` struct in `Deserialize` impl to include:

```rust
    #[serde(default)]
    io_mode: IoMode,
```

And the construction:

```rust
    io_mode: raw.io_mode,
```

**Step 4: Run tests**

Run: `cargo test model_spec`
Expected: PASS.

**Step 5: Run full test suite**

Run: `cargo test`
Expected: All existing tests pass (IoMode defaults to Pipe).

**Step 6: Commit**

```bash
git add src/model/spec.rs tests/
git commit -m "feat(pty): add IoMode enum and io_mode field to LaunchSpec"
```

---

### Task 2: Add PtyControl state and PTY metadata to Meta

**Files:**
- Modify: `src/model/meta.rs`
- Create: `src/model/pty.rs`
- Modify: `src/model/mod.rs`

**Step 1: Create the PtyControl types**

Create `src/model/pty.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Who currently controls input to a PTY session.
///
/// Slice one rules (no agent lease):
/// - start --pty → AgentControl
/// - attach → HumanControl (steals from AgentControl)
/// - human detach → AgentControl (always, no lease check)
/// - Detached only used if session started without --stdin
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PtyControl {
    /// Agent owns input — push is accepted.
    AgentControl,
    /// Human is attached — push is rejected, terminal relay is active.
    HumanControl,
    /// No one is connected (no push channel).
    Detached,
}

/// PTY session metadata. Present only for PTY-enabled sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyMeta {
    pub enabled: bool,
    pub control: PtyControl,
}

impl PtyMeta {
    pub fn new() -> Self {
        Self {
            enabled: true,
            control: PtyControl::AgentControl,
        }
    }
}
```

Add `pub mod pty;` to `src/model/mod.rs`.

**Step 2: Add pty field to Meta**

In `src/model/meta.rs`, add field:

```rust
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pty: Option<PtyMeta>,
```

Add accessor:

```rust
    #[must_use]
    pub fn pty(&self) -> Option<&PtyMeta> {
        self.pty.as_ref()
    }

    pub fn set_pty(&mut self, pty_meta: PtyMeta) {
        self.pty = Some(pty_meta);
    }

    pub fn set_pty_control(&mut self, control: PtyControl) {
        if let Some(ref mut p) = self.pty {
            p.control = control;
        }
    }
```

Update `new_starting` to initialize `pty: None`.

Update the `Raw` struct in `Deserialize` impl to include:

```rust
    #[serde(default)]
    pty: Option<PtyMeta>,
```

And construction: `pty: raw.pty,`

**Step 3: Write tests**

```rust
#[test]
fn meta_pty_none_by_default() {
    // Deserialize a meta without pty field — should be None
    // (use an existing test fixture or construct minimal JSON)
}

#[test]
fn meta_pty_serializes_when_present() {
    // Create meta, set_pty, serialize, check JSON has pty object
}
```

**Step 4: Run tests, commit**

```bash
git add src/model/pty.rs src/model/mod.rs src/model/meta.rs
git commit -m "feat(pty): add PtyControl state and PTY metadata to Meta"
```

---

### Task 3: Add `--pty` flag to CLI and wire to LaunchSpec

**Files:**
- Modify: `src/main.rs` (Cli struct, Commands::Start, remote_args)
- Modify: `src/commands/start.rs` (launch_session)

**Step 1: Write the failing test**

Create `tests/cli_pty.rs`:

```rust
#![cfg(unix)]

mod harness;

use std::sync::Mutex;
use harness::tender;
use tempfile::TempDir;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn start_pty_flag_accepted() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    let output = tender(&root)
        .args(["start", "pty-test", "--pty", "--", "echo", "hello"])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let meta: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(meta["launch_spec"]["io_mode"], "Pty");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test cli_pty start_pty_flag_accepted`
Expected: FAIL — `--pty` not recognized.

**Step 3: Add `--pty` flag and wire it**

In `src/main.rs`, add to `Commands::Start`:

```rust
    /// Interactive pseudo-terminal mode
    #[arg(long)]
    pty: bool,
```

In `src/commands/start.rs`, `launch_session()`, after building the launch_spec:

```rust
    if pty {
        launch_spec.io_mode = IoMode::Pty;
    }
```

Pass `pty` through `cmd_start` and `launch_session` function signatures.

In `src/main.rs`, update `Commands::remote_args()` for Start:

```rust
    if *pty { args.push("--pty".to_string()); }
```

In `src/main.rs`, update the match arm for `Commands::Start` in `main()` to pass `pty`.

**Step 4: Run tests**

Run: `cargo test --test cli_pty`
Expected: PASS (echo exits quickly, PTY spawn may fail until Task 5, but the meta should show io_mode).

Note: This test will initially pass because `echo` exits before the sidecar tries PTY-specific things. The actual PTY wiring comes in Task 5.

**Step 5: Commit**

```bash
git add src/main.rs src/commands/start.rs tests/cli_pty.rs
git commit -m "feat(pty): add --pty flag to start command"
```

---

### Task 4: Reject exec and guard push on PTY sessions

**Files:**
- Modify: `src/commands/exec.rs`
- Modify: `src/commands/push.rs`

**Step 1: Write the failing tests**

Add to `tests/cli_pty.rs`:

```rust
#[test]
fn exec_rejected_on_pty_session() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // Start a PTY session with stdin (needed for exec check)
    tender(&root)
        .args(["start", "pty-shell", "--pty", "--stdin", "--", "sleep", "60"])
        .output()
        .unwrap();
    harness::wait_running(&root, "pty-shell");

    let output = tender(&root)
        .args(["exec", "pty-shell", "--", "echo", "test"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not supported") || stderr.contains("PTY"),
        "should reject exec on PTY: {stderr}");

    // cleanup
    tender(&root).args(["kill", "pty-shell"]).output().ok();
}
```

**Step 2: Implement guards**

In `src/commands/exec.rs`, after the running check, add:

```rust
    if meta.launch_spec().io_mode == IoMode::Pty {
        anyhow::bail!("exec is not supported on PTY sessions");
    }
```

In `src/commands/push.rs`, the existing `stdin_mode` check handles
non-stdin sessions. For PTY sessions started with `--stdin`, push
should work (sidecar forwards FIFO to PTY). The human control check
comes later when the sidecar supports it.

**Step 3: Run tests, commit**

```bash
git add src/commands/exec.rs src/commands/push.rs tests/cli_pty.rs
git commit -m "feat(pty): reject exec on PTY sessions"
```

---

### Task 5: Platform PTY allocation — `spawn_child_pty`

This is the core task. Add PTY allocation to the Unix platform.

**Files:**
- Modify: `src/platform/mod.rs` (add trait method)
- Modify: `src/platform/unix.rs` (implement)
- Modify: `Cargo.toml` (add `termios` feature to rustix)

**Step 1: Add `termios` feature to rustix**

In `Cargo.toml`:

```toml
rustix = { version = "1.1.4", features = ["fs", "pipe", "process", "param", "termios", "pty"] }
```

If rustix doesn't have a `pty` feature, use `libc::openpty` directly (libc is already a dependency).

**Step 2: Extend SupervisedChild for PTY state**

In `src/platform/unix.rs`, modify `SupervisedChild`:

```rust
pub struct SupervisedChild {
    child: std::process::Child,
    identity: ProcessIdentity,
    /// Whether this child was spawned under a PTY.
    is_pty: bool,
    /// PTY master read half (returned by child_stdout). None for pipe sessions.
    pty_master_read: Option<File>,
    /// PTY master write half (returned by child_stdin). None for pipe sessions.
    pty_master_write: Option<File>,
}
```

The `is_pty` flag is an explicit marker — never inferred from Option
states. This avoids ambiguity after `child_stdout()` has been called
(which sets both pipe stdout and pty_master_read to None).

The PTY master fd is dup'd into two `File` handles: one for reading
(transcript capture, via `child_stdout`), one for writing (push/attach
input, via `child_stdin`). This fits the existing Platform trait —
the sidecar calls `child_stdout()` and `child_stdin()` the same way
it does for pipe sessions.

**Step 3: Add `spawn_child_pty` to Platform trait**

In `src/platform/mod.rs`, add to the trait:

```rust
    /// Spawn a child process under a pseudo-terminal.
    /// The child's stdin/stdout/stderr are all connected to the slave
    /// side of the PTY. The master is exposed through child_stdout()
    /// (read half) and child_stdin() (write half).
    ///
    /// child_stderr() returns None for PTY sessions (merged output).
    ///
    /// Unix only in slice one. Windows returns `Err`.
    fn spawn_child_pty(
        argv: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> io::Result<Self::SupervisedChild>;
```

**Step 4: Implement on Unix**

In `src/platform/unix.rs`:

```rust
fn spawn_child_pty(
    argv: &[String],
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> io::Result<SupervisedChild> {
    // 1. Create PTY pair
    let mut master_fd: libc::c_int = 0;
    let mut slave_fd: libc::c_int = 0;
    let ret = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: openpty returns valid fds on success.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

    // 2. Build command — stdin/stdout/stderr all point to slave
    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }

    // Set slave as stdin/stdout/stderr via pre_exec
    let slave_raw = slave.as_raw_fd();
    unsafe {
        cmd.pre_exec(move || {
            // New session — makes this process the session leader
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // Set controlling terminal
            if libc::ioctl(slave_raw, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // Dup slave to stdin/stdout/stderr
            if libc::dup2(slave_raw, 0) == -1 { return Err(io::Error::last_os_error()); }
            if libc::dup2(slave_raw, 1) == -1 { return Err(io::Error::last_os_error()); }
            if libc::dup2(slave_raw, 2) == -1 { return Err(io::Error::last_os_error()); }
            if slave_raw > 2 {
                libc::close(slave_raw);
            }
            // Process group (like non-PTY path)
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    // Child inherits slave via pre_exec dup2. Pipe nothing from Rust's side.
    cmd.stdin(std::process::Stdio::null())
       .stdout(std::process::Stdio::null())
       .stderr(std::process::Stdio::null());

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if !env.is_empty() {
        cmd.envs(env);
    }

    let child = cmd.spawn()?;

    // Close slave in parent — child owns it now
    drop(slave);

    // Dup master fd into two File handles: read half and write half.
    // Both reference the same underlying PTY master.
    let master_read = File::from(master.try_clone()?);
    let master_write = File::from(master);

    let pid = child.id();
    let identity = process_identity(pid)?;

    Ok(SupervisedChild {
        child,
        identity,
        is_pty: true,
        pty_master_read: Some(master_read),
        pty_master_write: Some(master_write),
    })
}
```

**Step 5: Update existing spawn_child**

Add `is_pty: false, pty_master_read: None, pty_master_write: None` to the return.

**Step 6: Update child_stdout / child_stdin / child_stderr for PTY**

In `child_stdout`: return `pty_master_read` if present, else pipe stdout.
In `child_stdin`: return `pty_master_write` if present, else pipe stdin.
In `child_stderr`: return `None` if PTY (merged output), else pipe stderr.

```rust
fn child_stdout(child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
    // PTY: return master read half. Pipe: return child stdout.
    if child.is_pty {
        child.pty_master_read.take()
            .map(|f| Box::new(f) as Box<dyn io::Read + Send>)
    } else {
        child.child.stdout.take()
            .map(|s| Box::new(s) as Box<dyn io::Read + Send>)
    }
}

fn child_stderr(child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
    // PTY: no stderr (merged into PTY master). Pipe: return child stderr.
    if child.is_pty {
        None
    } else {
        child.child.stderr.take()
            .map(|s| Box::new(s) as Box<dyn io::Read + Send>)
    }
}

fn child_stdin(child: &mut SupervisedChild) -> Option<Box<dyn io::Write + Send>> {
    // PTY: return master write half. Pipe: return child stdin.
    if child.is_pty {
        child.pty_master_write.take()
            .map(|f| Box::new(f) as Box<dyn io::Write + Send>)
    } else {
        child.child.stdin.take()
            .map(|s| Box::new(s) as Box<dyn io::Write + Send>)
    }
}
```

`is_pty` is checked explicitly — no inference from Option states.
The sidecar's `supervise()` function works unchanged for PTY:
`child_stdout()` returns the PTY master read, `child_stderr()` returns
`None`, so the sidecar spawns one capture thread (tag `O`) instead of two.

**Step 7: Add stub for Windows**

In `src/platform/windows.rs`, add:

```rust
fn spawn_child_pty(
    _argv: &[String],
    _cwd: Option<&Path>,
    _env: &BTreeMap<String, String>,
) -> io::Result<SupervisedChild> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "PTY not supported on Windows yet"))
}
```

**Step 8: Write test**

```rust
#[test]
#[cfg(unix)]
fn spawn_child_pty_creates_terminal() {
    use tender::platform::{Current, Platform};
    let child = Current::spawn_child_pty(
        &["tty".to_string()],
        None,
        &std::collections::BTreeMap::new(),
    );
    assert!(child.is_ok(), "spawn_child_pty should succeed: {:?}", child.err());
    let mut child = child.unwrap();
    let status = Current::child_wait(&mut child);
    assert!(status.is_ok());
    // tty command should exit 0 when it has a real terminal
    assert!(status.unwrap().success(), "tty should detect a terminal");
}
```

**Step 9: Run tests, commit**

```bash
git add Cargo.toml src/platform/mod.rs src/platform/unix.rs src/platform/windows.rs
git commit -m "feat(pty): add spawn_child_pty to Unix platform"
```

---

### Task 6: Wire sidecar to use PTY when io_mode is Pty

**Files:**
- Modify: `src/sidecar.rs`

**Step 1: Modify spawn path in run_inner**

In `src/sidecar.rs`, around line 466-484, replace the spawn block:

```rust
    let is_pty = meta.launch_spec().io_mode == IoMode::Pty;

    let mut child = if is_pty {
        match Current::spawn_child_pty(
            meta.launch_spec().argv(),
            meta.launch_spec().cwd.as_deref(),
            &effective_env,
        ) {
            Ok(c) => c,
            Err(e) => {
                // same SpawnFailed handling as current code
                ...
            }
        }
    } else {
        // existing pipe-based spawn
        ...
    };
```

**Step 2: Modify stdin forwarding — shared write handle for PTY**

For pipe sessions, this is unchanged: `child_stdin()` is passed
directly to the FIFO forwarding thread.

For PTY sessions, the write handle must be shared between FIFO
forwarding (push) and the attach listener (human input). So:

```rust
    // Take the write side once via the trait.
    let child_write = Current::child_stdin(&mut child);

    // For PTY: wrap in Arc<Mutex> for shared access.
    // For pipe: use directly (no sharing needed).
    let pty_write_handle: Option<Arc<Mutex<Box<dyn Write + Send>>>> = if is_pty {
        child_write.map(|w| Arc::new(Mutex::new(w)))
    } else {
        None
    };

    if meta.launch_spec().stdin_mode == StdinMode::Pipe {
        let stdin_target: Box<dyn Write + Send> = if let Some(ref shared) = pty_write_handle {
            // PTY: forwarding thread writes through the shared handle
            Box::new(SharedWriter(Arc::clone(shared)))
        } else {
            // Pipe: forwarding thread owns the write side directly
            child_write.ok_or_else(|| anyhow::anyhow!("child stdin not piped"))?
        };
        setup_stdin_forwarding(session_dir, stdin_target, &stdin_errors)?;
    }
```

`SharedWriter` is a thin wrapper that implements `Write` by locking
the `Arc<Mutex<...>>`. This keeps the forwarding thread's interface
unchanged while allowing the attach listener to share the same handle.

**Step 3: Supervision — existing `supervise()` works for PTY**

The existing `supervise()` function calls `child_stdout()` and
`child_stderr()`. For PTY sessions:

- `child_stdout()` returns the PTY master read half
- `child_stderr()` returns `None` (merged output)

Two changes needed:

**A. Handle optional stderr gracefully:**

```rust
    let stdout = Current::child_stdout(child).expect("stdout was piped");
    let stderr = Current::child_stderr(child); // None for PTY
```

**B. PTY capture must tee to both log and attached socket:**

For pipe sessions, `capture_stream` writes to `output.log` only.
For PTY sessions, the capture thread must also tee raw bytes to the
attached human's socket (if any). This is essential for interactive
use — prompts, TUIs, and partial lines cannot go through the
line-buffered log-follow path.

The capture thread receives an `Option<Arc<Mutex<UnixStream>>>`
representing the currently attached client. When a human attaches,
the attach listener sets this to `Some(stream)`. When they detach,
it's set back to `None`. The capture thread checks on each read:

```rust
fn capture_stream_pty(
    stream: Box<dyn Read + Send>,
    log: &Mutex<File>,
    attach_sink: &Mutex<Option<UnixStream>>,
) -> Result<(), String> {
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };

        // Write to log (best-effort line splitting for log queries).
        //
        // Known limitations in slice one:
        // - Non-UTF-8 bytes are replaced with U+FFFD (lossy conversion)
        // - Partial lines (no trailing newline) are buffered until the
        //   next read completes a line, or flushed as-is on EOF
        // - This is acceptable because PTY log is a transcript, not a
        //   structured data channel. The raw attach path (tee) is lossless.
        {
            let mut f = log.lock().map_err(|e| format!("log mutex: {e}"))?;
            let text = String::from_utf8_lossy(&buf[..n]);
            for line in text.lines() {
                let ts = timestamp_micros();
                write!(f, "{ts} O {line}\n").ok();
            }
        }

        // Tee raw bytes to attached client (if any)
        if let Ok(mut sink) = attach_sink.lock() {
            if let Some(ref mut stream) = *sink {
                // Send as Data message via attach protocol
                if attach_proto::write_msg(stream, MSG_DATA, &buf[..n]).is_err() {
                    // Client disconnected — clear the sink
                    *sink = None;
                }
            }
        }
    }
    Ok(())
}
```

The existing `capture_stream` stays for pipe sessions. PTY sessions
use `capture_stream_pty` which adds the tee. The `supervise()`
function picks the right one based on `is_pty`.

```rust
    let log_ref = &log;
    let attach_sink_ref = &attach_sink; // Arc<Mutex<Option<UnixStream>>>

    let (stdout_result, stderr_result) = std::thread::scope(|scope| {
        let stdout_handle = if is_pty {
            scope.spawn(move || capture_stream_pty(stdout, log_ref, attach_sink_ref))
        } else {
            scope.spawn(move || capture_stream(stdout, 'O', log_ref))
        };
        let stderr_handle = stderr.map(|s| scope.spawn(move || capture_stream(s, 'E', log_ref)));

        let stdout_r = stdout_handle.join()
            .unwrap_or_else(|_| Err("stdout capture thread panicked".into()));
        let stderr_r = stderr_handle
            .map(|h| h.join().unwrap_or_else(|_| Err("stderr capture thread panicked".into())))
            .unwrap_or(Ok(()));
        (stdout_r, stderr_r)
    });
```

This means the attach listener's output path is:
1. Human attaches → listener sets `attach_sink` to `Some(socket)`
2. Capture thread tees raw PTY bytes to the socket on every read
3. Human detaches → listener sets `attach_sink` to `None`

No log follow, no line buffering in the attach path. Raw bytes
from the PTY master go directly to the human's terminal.

**Step 5: Set PTY metadata on Meta**

After `meta.transition_running()`, if PTY:

```rust
    if is_pty {
        meta.set_pty(PtyMeta::new());
    }
```

**Step 6: Write integration test**

```rust
#[test]
fn start_pty_session_captures_output() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "pty-echo", "--pty", "--", "echo", "pty-hello"])
        .output()
        .unwrap();

    harness::wait_terminal(&root, "pty-echo");

    let output = tender(&root)
        .args(["log", "pty-echo", "--raw"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pty-hello"),
        "PTY output should be captured in log: {stdout}");
}

#[test]
fn start_pty_session_shows_pty_metadata() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "pty-meta", "--pty", "--", "echo", "hi"])
        .output()
        .unwrap();

    let output = tender(&root)
        .args(["status", "pty-meta"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let meta: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(meta["pty"]["enabled"], true);
    assert_eq!(meta["launch_spec"]["io_mode"], "Pty");
}
```

**Step 7: Run tests, commit**

```bash
git add src/sidecar.rs tests/cli_pty.rs
git commit -m "feat(pty): wire sidecar to PTY spawn and merged transcript capture"
```

---

### Task 7: Add `tender attach` command — Unix socket + terminal relay

**Files:**
- Create: `src/commands/attach.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/main.rs`
- Modify: `src/sidecar.rs` (add socket listener)

This is the largest task. It has two sides: the sidecar (socket listener + PTY relay) and the CLI command (raw terminal + socket client).

**Step 1: Define the wire protocol**

Create `src/attach_proto.rs`:

```rust
use std::io::{self, Read, Write};

/// Message types for the attach protocol.
/// Minimal framing: 1 byte type + 4 byte length + payload.
pub const MSG_DATA: u8 = 0x01;
pub const MSG_RESIZE: u8 = 0x02;
pub const MSG_DETACH: u8 = 0x03;

pub fn write_msg(w: &mut impl Write, msg_type: u8, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    w.write_all(&[msg_type])?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

pub fn read_msg(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    r.read_exact(&mut header)?;
    let msg_type = header[0];
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut payload)?;
    }
    Ok((msg_type, payload))
}

pub fn resize_payload(rows: u16, cols: u16) -> [u8; 4] {
    let mut buf = [0u8; 4];
    buf[0..2].copy_from_slice(&rows.to_be_bytes());
    buf[2..4].copy_from_slice(&cols.to_be_bytes());
    buf
}

pub fn parse_resize(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() < 4 { return None; }
    let rows = u16::from_be_bytes([payload[0], payload[1]]);
    let cols = u16::from_be_bytes([payload[2], payload[3]]);
    Some((rows, cols))
}
```

Add `pub mod attach_proto;` to `src/lib.rs`.

**Step 2: Add socket listener to sidecar**

Two data paths, both handled explicitly:

- **Human → PTY (input):** The listener reads framed messages from
  its read half of the socket. Data payloads are written to the
  shared PTY write handle (`Arc<Mutex<dyn Write + Send>>`). Resize
  messages trigger `ioctl TIOCSWINSZ`. This is the same shared handle
  that FIFO forwarding uses.

- **PTY → Human (output):** The capture thread tees raw PTY bytes
  to the attached socket via `attach_sink` (set up in Task 6). The
  listener does NOT read PTY output — the capture thread handles it.

**Socket duplex ownership:** A `UnixStream` is bidirectional, but
the listener needs to read from it (human input) while the capture
thread writes to it (PTY output) concurrently. Solution:
`try_clone()` the stream on connect. The listener owns the read
clone, the write clone goes into `attach_sink`:

```rust
    // On human connect:
    let write_half = stream.try_clone()?;
    let read_half = stream; // listener keeps this

    // Capture thread tees PTY output to the write half
    *attach_sink.lock().unwrap() = Some(write_half);

    // Listener reads framed input from the read half
    loop {
        match attach_proto::read_msg(&mut read_half) {
            Ok((MSG_DATA, payload)) => {
                pty_write.lock().unwrap().write_all(&payload).ok();
            }
            Ok((MSG_RESIZE, payload)) => {
                if let Some((rows, cols)) = attach_proto::parse_resize(&payload) {
                    // ioctl TIOCSWINSZ on the PTY
                }
            }
            Ok((MSG_DETACH, _)) | Err(_) => break,
            _ => {}
        }
    }

    // On disconnect: clear the sink, capture thread stops teeing
    *attach_sink.lock().unwrap() = None;
```

In `src/sidecar.rs`, after setting up stdin forwarding:

```rust
    if is_pty {
        let sock_path = session_dir.join("attach.sock");
        let pty_write = pty_write_handle.clone();
        let attach_sink_clone = Arc::clone(&attach_sink);
        let session_clone = session.clone();
        std::thread::spawn(move || {
            run_attach_listener(
                &sock_path, pty_write, attach_sink_clone, &session_clone,
            );
        });
    }
```

The listener thread:
- Binds a Unix domain socket at `attach.sock`
- Accepts one connection at a time
- On connect:
  - `try_clone()` the stream → read half (listener), write half (sink)
  - set `attach_sink` to `Some(write_half)` (capture thread tees output)
  - update meta `pty.control = HumanControl`, write to disk
- Input relay: listener reads framed messages from read half
- Output relay: capture thread tees raw PTY bytes to write half
- On disconnect:
  - set `attach_sink` to `None`
  - update meta `pty.control = AgentControl`, write to disk
- Clean up socket on session exit

Output flow:
```
PTY master → capture thread → tee → attach_sink write half (raw bytes)
                            → output.log (best-effort line-buffered)
```

**Step 3: Add `tender attach` CLI command**

Create `src/commands/attach.rs`:

```rust
pub fn cmd_attach(name: &str, namespace: &Namespace) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, namespace, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;

    if !matches!(meta.status(), RunStatus::Running { .. }) {
        anyhow::bail!("session is not running");
    }

    let pty = meta.pty()
        .ok_or_else(|| anyhow::anyhow!("session is not PTY-enabled"))?;

    if pty.control == PtyControl::HumanControl {
        anyhow::bail!("session is already under human control");
    }

    let sock_path = session.path().join("attach.sock");
    if !sock_path.exists() {
        anyhow::bail!("attach socket not found — session may not be fully started");
    }

    // Connect to socket
    let stream = std::os::unix::net::UnixStream::connect(&sock_path)?;

    // Put local terminal in raw mode
    let orig_termios = enter_raw_mode()?;
    let _guard = RawModeGuard(orig_termios);

    // Send initial resize
    send_resize(&stream)?;

    // Bidirectional relay: local terminal ↔ socket
    relay(stream)?;

    Ok(())
}
```

**Step 4: Wire into CLI**

In `src/main.rs`, add `Attach` variant to `Commands` enum:

```rust
    /// Attach to a PTY session's terminal
    Attach {
        /// Session name
        name: String,
        /// Namespace
        #[arg(long)]
        namespace: Option<String>,
    },
```

Add dispatch in `main()` and `remote_args()`.

Update SSH allowlist in `src/ssh.rs` to include `"attach"`.

Update `build_ssh_command` to use `-t` when the command is `attach`.

**Step 5: Write integration test**

Testing attach interactively is tricky. A basic test:

```rust
#[test]
fn attach_to_non_pty_session_fails() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "pipe-session", "--", "sleep", "60"])
        .output()
        .unwrap();
    harness::wait_running(&root, "pipe-session");

    let output = tender(&root)
        .args(["attach", "pipe-session"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not PTY"), "stderr: {stderr}");

    tender(&root).args(["kill", "pipe-session"]).output().ok();
}

#[test]
fn attach_socket_exists_for_pty_session() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["start", "pty-attach", "--pty", "--", "sleep", "60"])
        .output()
        .unwrap();
    harness::wait_running(&root, "pty-attach");

    let sock = root.path()
        .join(".tender/sessions/default/pty-attach/attach.sock");
    assert!(sock.exists(), "attach.sock should exist for PTY session");

    tender(&root).args(["kill", "pty-attach"]).output().ok();
}
```

**Step 6: Commit**

```bash
git add src/attach_proto.rs src/commands/attach.rs src/commands/mod.rs
git add src/main.rs src/ssh.rs src/sidecar.rs src/lib.rs tests/cli_pty.rs
git commit -m "feat(pty): add attach command with Unix socket relay"
```

---

### Task 8: Push on PTY sessions — FIFO to PTY forwarding

**Files:**
- Modify: `tests/cli_pty.rs`

This should already work from Task 6's sidecar wiring (FIFO → PTY master).
This task adds explicit test coverage.

**Step 1: Write the test**

```rust
#[test]
fn push_to_pty_session_delivers_input() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = TempDir::new().unwrap();

    // Start a PTY cat session with stdin
    tender(&root)
        .args(["start", "pty-push", "--pty", "--stdin", "--", "cat"])
        .output()
        .unwrap();
    harness::wait_running(&root, "pty-push");

    // Push some input
    let mut push = tender(&root);
    push.args(["push", "pty-push"]);
    push.write_stdin(b"hello-from-push\n");
    push.output().unwrap();

    // Give cat time to echo
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Check log
    let output = tender(&root)
        .args(["log", "pty-push", "--raw"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello-from-push"),
        "push input should appear in PTY log: {stdout}");

    tender(&root).args(["kill", "pty-push"]).output().ok();
}
```

**Step 2: Run test, commit**

```bash
git add tests/cli_pty.rs
git commit -m "test(pty): verify push delivers input to PTY sessions"
```

---

### Task 9: SSH dispatch — attach uses `-t` for TTY allocation

**Files:**
- Modify: `src/ssh.rs`
- Modify: `tests/cli_remote.rs`

**Step 1: Update build_ssh_command**

In `src/ssh.rs`, modify `build_ssh_command`:

```rust
pub fn build_ssh_command(host: &str, tender_args: &[String], allocate_tty: bool) -> Command {
    let mut cmd = Command::new("ssh");
    let tty_flag = if allocate_tty { "-t" } else { "-T" };
    cmd.args([tty_flag, "-o", "ConnectTimeout=10", host]);
    // ... rest unchanged
}
```

Update `exec_ssh` to pass `allocate_tty`:

```rust
pub fn exec_ssh(host: &str, tender_args: &[String], allocate_tty: bool) -> Result<i32, SshError> {
    let mut child = build_ssh_command(host, tender_args, allocate_tty)
        // ...
}
```

In `src/main.rs`, the dispatch:

```rust
    let allocate_tty = cmd_name == "attach";
    match tender::ssh::exec_ssh(host, &args, allocate_tty) {
```

**Step 2: Update existing tests**

All existing `cli_remote.rs` tests that call `parse_remote_argv` will need
to account for the first arg being `-T` or `-t`. The tests already check
for `-T` at index 0 — those remain correct for non-attach commands.

Add a test:

```rust
#[test]
fn host_flag_attach_uses_tty_allocation() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "attach", "my-session"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let args: Vec<&str> = stdout.lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .collect();
    assert!(args.contains(&"-t"), "attach should use -t: {args:?}");
    assert!(!args.contains(&"-T"), "attach should not use -T: {args:?}");
}
```

**Step 3: Commit**

```bash
git add src/ssh.rs src/main.rs tests/cli_remote.rs
git commit -m "feat(pty): SSH attach uses -t for TTY allocation"
```

---

### Task 10: Full test suite verification and cleanup

**Step 1: Run all tests**

Run: `cargo test`
Expected: All tests pass.

**Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No new warnings from our code.

**Step 3: Commit any fixes**

```bash
git commit -m "fix: address clippy warnings from PTY implementation"
```

---

## Implementation notes

**Task ordering dependencies:**

```
Task 1 (IoMode)
  → Task 2 (PtyMeta)
    → Task 3 (--pty flag)
      → Task 4 (exec/push guards)
Task 5 (spawn_child_pty) — independent of Tasks 1-4
Task 1 + Task 5
  → Task 6 (sidecar wiring) — needs both IoMode and PTY spawn
    → Task 7 (attach) — needs sidecar socket
    → Task 8 (push test) — needs sidecar FIFO→PTY
Task 7
  → Task 9 (SSH -t) — needs attach command to exist
All tasks
  → Task 10 (verification)
```

**Parallelizable:** Tasks 1-4 (model + CLI) and Task 5 (platform) are independent.

## What is NOT in this plan

- Windows ConPTY support
- PTY-backed exec
- Observe-only attach
- Agent lease / heartbeat / lease expiry (slice one: detach always → AgentControl)
- Terminal recording / replay
- Attach from `watch` events
