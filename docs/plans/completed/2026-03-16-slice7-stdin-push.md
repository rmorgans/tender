# Slice 7: Stdin Push Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `tender push <session>` reads stdin and delivers it to the supervised child's stdin via a named pipe (mkfifo).

**Architecture:** The sidecar creates a mkfifo at `stdin.pipe` in the session dir when `stdin_mode == Pipe`, **before signaling readiness**. It spawns a forwarding thread that opens the fifo read end (blocking until a writer connects), then copies bytes from fifo to child stdin in a loop. When a writer disconnects, the thread re-opens the fifo to accept the next writer. The CLI `push` command opens the write end, copies its own stdin into it, and exits. Multiple sequential pushes are supported. Concurrent pushes will interleave bytes at the FIFO level — this is acceptable for the agent use case where pushes are sequential commands.

**Tech Stack:** Rust std only (`libc::mkfifo`). No new dependencies.

**Key constraints:**
- FIFO creation and forwarding thread launch happen **before readiness signal** — `tender push` immediately after `tender start --stdin` must not race
- The child's stdin is `Stdio::piped()` when `stdin_mode == Pipe`, `Stdio::null()` when `None`
- Push requires `RunStatus::Running` explicitly — not just "non-terminal"
- Push to a session with `stdin_mode == None` is an error (detectable from meta.json)
- The forwarding thread is effectively detached — not joined before `supervise`. The sidecar process exiting is what terminates it. FIFO removal is cleanup, not the shutdown mechanism.

---

## Task 1: Platform mkfifo helper

**Files:**
- Modify: `src/platform/unix.rs`

Add:

```rust
/// Create a named pipe (FIFO) at `path` with mode 0600.
pub fn mkfifo(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))?;
    let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
```

Thin libc wrapper — integration tests cover it.

**Commit:** `feat(platform): add mkfifo helper`

---

## Task 2: Sidecar stdin pipe support

**Files:**
- Modify: `src/sidecar.rs`
- Modify: `src/model/spec.rs` (add `stdin_mode()` getter if needed)

Changes to `run_inner`, in order:

### 2a: Conditional child stdin

Change child spawn from hardcoded `Stdio::null()` to conditional based on `stdin_mode`:

```rust
use crate::model::spec::StdinMode;

if *meta.launch_spec().stdin_mode() == StdinMode::Pipe {
    cmd.stdin(Stdio::piped());
} else {
    cmd.stdin(Stdio::null());
}
```

If `LaunchSpec` doesn't have a `stdin_mode()` getter, either add one or access the public field directly (it's currently `pub`). Check and be consistent.

### 2b: FIFO creation before readiness

After getting child identity and transitioning to Running, but **before** `signal_meta_snapshot`:

```rust
// Create stdin FIFO and start forwarding thread BEFORE signaling readiness.
// This ensures tender push immediately after tender start --stdin doesn't race.
if meta.launch_spec().stdin_mode == StdinMode::Pipe {
    let fifo_path = session_dir.join("stdin.pipe");
    platform::mkfifo(&fifo_path)?;

    // Take child stdin — we'll forward fifo data into it
    let child_stdin = child.stdin.take()
        .ok_or_else(|| anyhow::anyhow!("child stdin not piped"))?;

    // Spawn forwarding thread (detached — not joined)
    let fifo_path_clone = fifo_path.clone();
    std::thread::spawn(move || forward_stdin(fifo_path_clone, child_stdin));
}
```

Then readiness signal happens as before. The FIFO exists and has a reader thread waiting before `tender start` returns.

### 2c: Forwarding function

```rust
/// Forward data from the stdin FIFO to the child's stdin pipe.
/// Re-opens the FIFO after each writer disconnects to support multiple pushes.
/// Exits when: child stdin write fails (child exited) or FIFO open fails (removed).
fn forward_stdin(fifo_path: PathBuf, mut child_stdin: std::process::ChildStdin) {
    let mut buf = [0u8; 8192];
    loop {
        // Open blocks until a writer connects
        let mut fifo = match File::open(&fifo_path) {
            Ok(f) => f,
            Err(_) => return, // FIFO removed or error — exit
        };
        // Copy until writer disconnects (read returns 0) or child stdin breaks
        loop {
            let n = match fifo.read(&mut buf) {
                Ok(0) => break,       // writer disconnected
                Ok(n) => n,
                Err(_) => return,     // read error — exit
            };
            if child_stdin.write_all(&buf[..n]).is_err() {
                return; // child stdin closed — child exited
            }
        }
        // Writer disconnected. Loop to accept next writer.
    }
}
```

### 2d: Cleanup on exit

After `supervise` returns, before writing terminal state:

```rust
let _ = std::fs::remove_file(session_dir.join("stdin.pipe"));
```

**Commit:** `feat(sidecar): create stdin FIFO before readiness, forward to child`

---

## Task 3: CLI push command

**Files:**
- Modify: `src/main.rs`

Add `Push` variant to `Commands`:
```rust
/// Send stdin to a running session's child process
Push {
    /// Session name
    name: String,
},
```

Add match arm in `main()`:
```rust
Commands::Push { name } => cmd_push(&name),
```

Implement `cmd_push`:
```rust
fn cmd_push(name: &str) -> anyhow::Result<()> {
    use tender::model::ids::SessionName;
    use tender::model::spec::StdinMode;
    use tender::model::state::RunStatus;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;

    // Push requires Running state explicitly
    if !matches!(meta.status(), RunStatus::Running { .. }) {
        anyhow::bail!("session is not running (status: {})", meta.status_name());
    }

    if meta.launch_spec().stdin_mode != StdinMode::Pipe {
        anyhow::bail!("session was not started with --stdin");
    }

    let fifo_path = session.path().join("stdin.pipe");

    // Open FIFO write end — connects to sidecar's forwarding thread
    let mut fifo = std::fs::OpenOptions::new()
        .write(true)
        .open(&fifo_path)
        .map_err(|e| anyhow::anyhow!("failed to open stdin pipe: {e}"))?;

    // Copy our stdin to the FIFO
    let mut stdin = std::io::stdin().lock();
    std::io::copy(&mut stdin, &mut fifo)?;

    Ok(())
}
```

Note: `meta.status_name()` may not exist. If not, use a simple match or format `{:?}`. Check `RunStatus` for existing display support.

**Commit:** `feat(cli): add tender push command`

---

## Task 4: Wire --stdin flag to start command

**Files:**
- Modify: `src/main.rs`

Add `--stdin` flag to `Start` variant:
```rust
/// Enable stdin pipe for push command
#[arg(long)]
stdin: bool,
```

Update `cmd_start` to use it:
```rust
launch_spec.stdin_mode = if stdin { StdinMode::Pipe } else { StdinMode::None };
```

**Commit:** `feat(cli): add --stdin flag to tender start`

---

## Task 5: Integration tests

**Files:**
- Create: `tests/cli_push.rs`

Helper functions (same pattern as other test files):
```rust
fn tender_bin() -> PathBuf { ... }
fn run_tender(root: &TempDir, args: &[&str]) -> Output { ... }
fn wait_running(root: &TempDir, session: &str) { ... }
fn wait_terminal(root: &TempDir, session: &str) { ... }

/// Run tender with piped stdin input
fn run_tender_stdin(root: &TempDir, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(tender_bin())
        .args(args)
        .env("HOME", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn tender");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
```

Tests:

1. **push_delivers_stdin_to_child**: `tender start --stdin cat-job cat`, wait running, push "hello\n", kill, wait terminal, check output.log contains "hello"

2. **push_multiple_sequential**: `tender start --stdin cat-multi cat`, wait running, push "first\n", push "second\n", kill, wait terminal, check log contains both

3. **push_to_session_without_stdin_fails**: `tender start echo-job echo hi` (no --stdin), wait running... actually echo exits immediately so wait terminal, then push → error

4. **push_to_nonexistent_session_fails**: `tender push nope` → non-zero exit

5. **push_to_terminal_session_fails**: `tender start --stdin term-job true`, wait terminal, push → error (not running)

6. **push_immediately_after_start**: `tender start --stdin imm-job cat`, then immediately push "immediate\n" (no wait_running), kill, check log contains "immediate". Tests the readiness contract — FIFO must exist when start returns.

**Commit:** `test(push): add integration tests for stdin push`

---

## Task 6: Full suite verification

Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. All green.

---

## Summary

| Task | What | Tests |
|------|------|-------|
| 1 | Platform mkfifo | 0 |
| 2 | Sidecar FIFO + forwarding | 0 |
| 3 | CLI push command | 0 |
| 4 | --stdin flag on start | 0 |
| 5 | Integration tests | 6 |
| 6 | Verification | 0 |

**Total new tests:** 6 integration
**Modified files:** `src/platform/unix.rs`, `src/sidecar.rs`, `src/main.rs`, possibly `src/model/spec.rs`
**New files:** `tests/cli_push.rs`
**No new dependencies.**
