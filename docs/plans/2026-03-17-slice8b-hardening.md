# Slice 8B: Model-Runtime Convergence Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Close the remaining gaps between the typed state model and the runtime behavior so that "invalid states unrepresentable" is true for timestamps, degradation visibility, and all exit reason variants.

**Architecture:** Seven targeted changes, each independently testable, ordered from simplest to most invasive. No new commands or subcommands — this is purely tightening existing paths.

**Tech Stack:** Rust std only. No new dependencies.

---

## Task 1: Typed readiness snapshot in cmd_start

**Files:**
- Modify: `src/main.rs`

**Problem:** `cmd_start` at lines 261-267 parses the readiness snapshot as `serde_json::Value` and branches on `meta.get("status").as_str() == Some("SpawnFailed")`. The rest of the codebase uses `Meta` + `RunStatus`.

**Fix:** Deserialize the pipe snapshot directly to `Meta`, use `status().is_terminal()` and match on `RunStatus::SpawnFailed`:

```rust
// Replace lines 261-267:
let meta: tender::model::meta::Meta = serde_json::from_str(meta_json)?;
let json = serde_json::to_string_pretty(&meta)?;
println!("{json}");

if matches!(meta.status(), tender::model::state::RunStatus::SpawnFailed { .. }) {
    std::process::exit(2);
}
```

**Test:** Existing tests already cover SpawnFailed (sidecar_ready, cli_wait). Verify no regressions.

**Commit:** `fix: type readiness snapshot as Meta instead of raw JSON in cmd_start`

---

## Task 2: Push hang mitigation with non-blocking FIFO open

**Files:**
- Modify: `src/main.rs` (cmd_push)
- Modify: `src/platform/unix.rs` (add open_fifo_write_nonblock)

**Problem:** `cmd_push` at line 297-300 does a blocking `open(...write)` on the FIFO. If the forwarding thread dies between the meta check and the open, push blocks indefinitely.

**Fix:** Use `O_NONBLOCK | O_WRONLY` to open the FIFO. If no reader is connected, this returns `ENXIO` immediately instead of blocking. Retry with a short poll loop + meta liveness recheck. If the session goes terminal during the retry, error promptly.

Add to `src/platform/unix.rs`:
```rust
/// Open a FIFO for writing without blocking. Returns ENXIO if no reader.
pub fn open_fifo_write_nonblock(path: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Clear O_NONBLOCK after connect so writes block normally
    unsafe { libc::fcntl(fd, libc::F_SETFL, 0) };
    Ok(unsafe { File::from_raw_fd(fd) })
}
```

Update `cmd_push` to use retry loop:
```rust
let mut fifo = loop {
    match platform::open_fifo_write_nonblock(&fifo_path) {
        Ok(f) => break f,
        Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
            // No reader — check if session is still running
            let current = session::read_meta(&session)?;
            if !matches!(current.status(), RunStatus::Running { .. }) {
                anyhow::bail!("session exited before push could connect");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(e) => return Err(e.into()),
    }
};
```

**Test:** Add `push_fails_promptly_when_session_dies` to `tests/cli_push.rs`: start `--stdin sleep 1`, wait ~1.5s (child exits), then push → should error promptly, not hang.

**Commit:** `fix: non-blocking FIFO open in push with liveness recheck`

---

## Task 3: Warnings field in Meta

**Files:**
- Modify: `src/model/meta.rs`

**Problem:** Capture errors and stdin forwarding failures are invisible in meta.json. Agents can't detect degraded runs.

**Fix:** Add a `warnings` field to `Meta`:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
warnings: Vec<String>,
```

Add accessor and mutator:
```rust
pub fn warnings(&self) -> &[String] {
    &self.warnings
}

pub fn add_warning(&mut self, msg: String) {
    self.warnings.push(msg);
}
```

Initialize as empty in `new_starting`. Update the `Raw` struct in the custom `Deserialize` impl to include `#[serde(default)] warnings: Vec<String>`.

**Test:** Unit test: serialize Meta with warnings, deserialize, verify roundtrip. Test that empty warnings are not serialized (skip_serializing_if).

**Commit:** `feat: add warnings field to Meta for degradation visibility`

---

## Task 4: Report capture/stdin failures into Meta warnings

**Files:**
- Modify: `src/sidecar.rs`

**Problem:** `capture_errors.log` is a side file. `forward_stdin` failures are completely silent.

**Fix:**

4a: After `supervise` returns in `run_inner`, read `capture_errors.log` if it exists, add each error as a warning to meta before writing terminal state:
```rust
let capture_err_path = session_dir.join("capture_errors.log");
if let Ok(errors) = std::fs::read_to_string(&capture_err_path) {
    for line in errors.lines() {
        if !line.is_empty() {
            meta.add_warning(format!("log capture: {line}"));
        }
    }
}
```

4b: For stdin forwarding, use an `Arc<Mutex<Vec<String>>>` shared with the forwarding thread. The thread pushes error messages. After supervise, drain and add to warnings:
```rust
// Before spawning thread:
let stdin_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
let stdin_errors_clone = Arc::clone(&stdin_errors);

// In forward_stdin, catch errors:
fn forward_stdin(fifo_path: PathBuf, mut child_stdin: ChildStdin, errors: Arc<Mutex<Vec<String>>>) {
    // ... on error:
    if let Ok(mut errs) = errors.lock() {
        errs.push(format!("stdin forwarding failed: {e}"));
    }
    return;
}

// After supervise, before writing terminal state:
if let Ok(errs) = stdin_errors.lock() {
    for e in errs.iter() {
        meta.add_warning(e.clone());
    }
}
```

**Test:** Hard to test capture failures in integration. Add a unit test for the warnings → meta → JSON roundtrip. The integration tests for push already verify the happy path.

**Commit:** `feat: report capture and stdin-forwarding failures as meta warnings`

---

## Task 5: --timeout and TimedOut

**Files:**
- Modify: `src/sidecar.rs` (supervise)
- Modify: `src/model/spec.rs` (timeout_s is already in LaunchSpec)
- Modify: `src/main.rs` (add --timeout flag to Start)

**Problem:** `ExitReason::TimedOut` exists in the model but is unreachable. `timeout_s` exists in `LaunchSpec` but is never read.

**Fix:**

5a: Add `--timeout` flag to `Start` in main.rs:
```rust
/// Kill child after N seconds
#[arg(long)]
timeout: Option<u64>,
```
Wire it into LaunchSpec: `launch_spec.timeout_s = timeout;`

5b: In `supervise`, if `timeout_s` is set, use a timeout thread:
- Spawn a thread that sleeps for `timeout_s` seconds then kills the child
- If child exits before timeout, the thread is harmless (kill returns error on dead child)
- If timeout fires, the sidecar maps the exit to `ExitReason::TimedOut`

The tricky part: the sidecar needs to know whether the timeout fired or the child exited normally. Use an `AtomicBool` flag:

```rust
let timed_out = Arc::new(AtomicBool::new(false));

if let Some(timeout_s) = meta.launch_spec().timeout_s {
    let timed_out_clone = Arc::clone(&timed_out);
    let child_pid = child.id();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(timeout_s));
        timed_out_clone.store(true, Ordering::Relaxed);
        unsafe { libc::kill(child_pid as i32, libc::SIGKILL); }
    });
}
```

Then in the exit reason mapping:
```rust
let reason = if timed_out.load(Ordering::Relaxed) {
    ExitReason::TimedOut
} else {
    match status.code() {
        Some(0) => ExitReason::ExitedOk,
        Some(code) => ExitReason::ExitedError { code: NonZeroI32::new(code).expect("...") },
        None => ExitReason::Killed,
    }
};
```

**Tests:**
1. `timeout_kills_child`: start `sleep 60 --timeout 2`, wait → TimedOut in ~2s
2. `timeout_not_triggered`: start `true --timeout 60`, wait → ExitedOk (timeout doesn't fire)

**Commit:** `feat: implement --timeout with TimedOut exit reason`

---

## Task 6: KilledForced reachability

**Files:**
- Modify: `src/sidecar.rs`
- Modify: `src/platform/unix.rs` (optional: add signal file)

**Problem:** `ExitReason::KilledForced` exists but the sidecar always maps signal death to `Killed`. The sidecar doesn't know whether SIGTERM or SIGKILL was the killing signal.

**Fix:** Use a signal file in the session dir. When `kill_process` escalates to SIGKILL (in `platform/unix.rs` line 273), write a marker file `kill_forced` to the session dir. The sidecar checks for this file when mapping exit reasons.

However, `kill_process` doesn't know the session dir. Simpler approach: the CLI `cmd_kill --force` writes the marker file before calling `kill_process`:

In `cmd_kill`:
```rust
if force {
    let _ = std::fs::write(session.path().join("kill_forced"), "");
}
platform::kill_process(&child, force)?;
```

In `sidecar.rs`, exit reason mapping:
```rust
None => {
    if session_dir.join("kill_forced").exists() {
        let _ = std::fs::remove_file(session_dir.join("kill_forced"));
        ExitReason::KilledForced
    } else {
        ExitReason::Killed
    }
}
```

**Tests:**
1. `force_kill_produces_killed_forced`: start `sleep 60`, kill --force, wait → reason is KilledForced
2. `graceful_kill_produces_killed`: start `sleep 60`, kill (no --force), wait → reason is Killed

**Commit:** `feat: make KilledForced reachable via kill_forced marker file`

---

## Task 7: Timestamp newtype

**Files:**
- Modify: `src/model/ids.rs` (add EpochTimestamp)
- Modify: `src/model/state.rs` (change ended_at types)
- Modify: `src/model/meta.rs` (change started_at type)
- Modify: `src/model/transition.rs` (accept EpochTimestamp)
- Modify: `src/sidecar.rs` (produce EpochTimestamp)
- Modify: `src/main.rs` (produce EpochTimestamp in reconciliation)

**Problem:** `started_at` and `ended_at` are `String` — any text passes deserialization.

**Fix:** Add `EpochTimestamp` newtype:
```rust
/// Epoch seconds as a validated timestamp. Cannot be zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochTimestamp(u64);

impl EpochTimestamp {
    pub fn now() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self(secs)
    }

    pub fn as_secs(&self) -> u64 {
        self.0
    }
}
```

Serialize as a number (u64), deserialize with validation (non-zero):
```rust
impl Serialize for EpochTimestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for EpochTimestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Accept both string and number for backwards compatibility
        // ... deserialize, validate non-zero
    }
}
```

**Important:** This is a schema change. Existing meta.json files have string timestamps like `"1773653954"`. The deserializer must accept both string and integer formats for backwards compatibility. Serialize as integer going forward.

Replace all `started_at: String` and `ended_at: String` with `EpochTimestamp`. Replace `now_epoch_secs() -> String` with `EpochTimestamp::now()`.

**Tests:** Unit tests for EpochTimestamp: roundtrip, backwards-compat string parsing, reject invalid. Existing integration tests verify no regressions.

**Commit:** `feat: replace string timestamps with validated EpochTimestamp newtype`

---

## Task 8: Full suite verification

Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. All green.

---

## Summary

| Task | What | Category |
|------|------|----------|
| 1 | Typed readiness snapshot | Must Fix |
| 2 | Push hang mitigation | Must Fix |
| 3 | Warnings field in Meta | Must Fix |
| 4 | Capture/stdin failure reporting | Must Fix |
| 5 | --timeout + TimedOut | Model Convergence |
| 6 | KilledForced reachability | Model Convergence |
| 7 | Timestamp newtype | Model Convergence |
| 8 | Verification | Polish |

**Deferred:** Generation increment on --replace (debug counter, no runtime impact).
