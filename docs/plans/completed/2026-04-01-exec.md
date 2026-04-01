---
id: exec
depends_on:
  - wrap-annotation-ingestion
links: []
---

# tender exec — Structured Commands In A Persistent Shell

Run commands inside an already-running shell session and get structured results back: stdout, stderr, exit code, and final cwd.

`exec` is the missing layer between:

- `start --stdin`, which gives you a persistent shell
- `push`, which only writes raw bytes into that shell

## First Slice Goal

Land a safe, serialized `exec` for non-PTY shell sessions that were started with `--stdin`.

First-slice contract:

- exactly one `exec` runs against a session at a time
- the target session must already exist and be `running`
- the command is framed and sent through the existing stdin transport
- completion is detected from `output.log` via a unique sentinel line
- the caller gets stdout, stderr, exit code, and final cwd
- an annotation event is written with the structured result

This slice is for agent workflows, not arbitrary terminal emulation.

## CLI

```bash
tender exec <session> [--namespace <ns>] -- <command> [args...]
tender exec <session> [--namespace <ns>] [--timeout 30] -- <command> [args...]
```

Examples:

```bash
tender start shell --stdin -- /bin/bash
tender exec shell -- pwd
tender exec shell -- cd repo
tender exec shell -- cargo test
```

The shell stays alive between calls, so cwd and exported env persist.

## Required Session Properties

The target session must satisfy all of these:

- session exists
- session status is `Running`
- stdin transport is enabled (`--stdin`)
- session is a line-oriented shell session
- session is not PTY-backed

If these are not true, `exec` fails before pushing anything.

## Protocol

`exec` is implemented as:

1. capture a log cursor from the session's `output.log`
2. serialize the requested argv into one shell command string
3. wrap it with a unique sentinel trailer
4. send the framed command through the existing stdin transport
5. tail `output.log` from the captured cursor until the sentinel appears
6. parse stdout/stderr from the tagged log lines
7. extract exit code and final cwd from the sentinel line
8. emit an annotation event
9. return the structured result to the caller

## First-Cut Sentinel Format

The sentinel line must carry:

- random token
- exit code
- final cwd

Unix shell framing:

```sh
<user command>; status=$?; cwd_now="$(pwd)"; printf '__TENDER_EXEC__ %s %s %s\n' "$token" "$status" "$cwd_now"
```

PowerShell framing:

```powershell
<user command>; $status=$LASTEXITCODE; $cwd=(Get-Location).Path; Write-Output "__TENDER_EXEC__ $token $status $cwd"
```

This is enough for the first slice because:

- it preserves shell state
- it makes final cwd observable
- it uses only text output, which matches current `output.log` handling

## Concurrency Model

`exec` is single-flight per session in the first slice.

Implementation rule:

- create an advisory lock such as `exec.lock` in the session dir
- if another `exec` is active, fail immediately with a busy error
- do not attempt to queue or interleave commands

This avoids one caller consuming another caller's sentinel and keeps log parsing tractable.

## Output Model

`exec` reads from the current end of `output.log`, not from the beginning of the session.

Returned result:

- `stdout`: concatenated `O` lines after the log cursor
- `stderr`: concatenated `E` lines after the log cursor
- `exit_code`
- `cwd_after`
- `truncated`

The sentinel line itself is not included in stdout/stderr.

## Timeout Model

First-slice timeout is client-side only.

Meaning:

- `tender exec --timeout 30` stops waiting after 30 seconds
- the shell session remains alive
- the in-shell command may still be running
- the user gets an explicit timeout error that says execution may still be in progress

This is a deliberate first-slice tradeoff. Killing only the currently running in-shell command without killing the shell is follow-on work.

## Binary Output

First slice is text-oriented.

That means:

- binary-heavy commands are unsupported
- sentinel scanning assumes text output
- if binary-safe command execution becomes important later, it should be a protocol revision, not complexity added to slice one

## Annotation Payload

`exec` should write the same top-level envelope shape used by `wrap`, with `event: "exec"` and structured command metadata:

```json
{
  "source": "agent.hooks",
  "event": "exec",
  "run_id": "...",
  "data": {
    "hook_stdin": "cargo test",
    "hook_stdout": "...",
    "hook_stderr": "...",
    "hook_exit_code": 0,
    "command": ["cargo", "test"],
    "cwd_after": "/work/repo",
    "sentinel": "TENDER_EXEC_<uuid>",
    "timed_out": false,
    "truncated": false
  }
}
```

## Watch Integration

Watch should continue to expose both layers:

- raw output lines as `log` events from the sidecar
- structured `exec` results as `annotation` events

`exec` should not invent a second event transport.

## Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `tender exec` — run structured commands in an already-running shell session and get stdout, stderr, exit code, and final cwd back.

**Architecture:** `exec` is a CLI-only command (no sidecar changes). It acquires a per-session advisory lock, captures a log cursor, frames the user command with a sentinel trailer, pushes it through the existing stdin transport, tails `output.log` until the sentinel appears, parses the result, writes an annotation, and returns structured JSON.

**Tech Stack:** Rust, clap (CLI), flock/LockFileEx (advisory lock), existing stdin transport + output.log infrastructure

---

### Task 1: CLI command + preflight checks + exec lock

**Files:**
- Create: `src/commands/exec.rs`
- Modify: `src/commands/mod.rs` — add `pub mod exec;` and `pub use exec::cmd_exec;`
- Modify: `src/main.rs` — add `Exec` command variant

**Step 1: Add `Exec` to CLI**

In `src/main.rs`, add after the `Wait` command:

```rust
/// Execute a command in a running shell session
Exec {
    /// Session name
    name: String,
    /// Namespace for session grouping
    #[arg(long)]
    namespace: Option<String>,
    /// Timeout in seconds (client-side only)
    #[arg(long)]
    timeout: Option<u64>,
    /// Command and arguments
    #[arg(trailing_var_arg = true, required = true)]
    cmd: Vec<String>,
},
```

Add the match arm in `main()`:

```rust
Commands::Exec {
    name,
    namespace,
    timeout,
    cmd,
} => resolve_namespace(namespace)
    .and_then(|ns| commands::cmd_exec(&name, cmd, timeout, &ns)),
```

**Step 2: Create `src/commands/exec.rs` with preflight and lock**

```rust
use std::fs::File;
use std::io::Write;

use tender::model::ids::{Namespace, SessionName};
use tender::model::spec::StdinMode;
use tender::model::state::RunStatus;
use tender::session::{self, SessionDir, SessionRoot};

/// Advisory lock for serializing exec calls on a session.
/// Uses flock (Unix) / LockFileEx (Windows) — same pattern as session::LockGuard.
struct ExecLock {
    _file: File,
}

impl ExecLock {
    fn try_acquire(session: &SessionDir) -> anyhow::Result<Self> {
        let lock_path = session.path().join("exec.lock");
        let file = File::create(&lock_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let ret =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    anyhow::bail!("another exec is already running on this session");
                }
                return Err(err.into());
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
            use windows_sys::Win32::Storage::FileSystem::{
                LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
            };

            let handle = file.as_raw_handle() as _;
            let mut overlapped = unsafe { std::mem::zeroed() };
            let ret = unsafe {
                LockFileEx(
                    handle,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            };
            if ret == 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                    anyhow::bail!("another exec is already running on this session");
                }
                return Err(err.into());
            }
        }

        Ok(Self { _file: file })
    }
}

pub fn cmd_exec(
    name: &str,
    cmd: Vec<String>,
    timeout: Option<u64>,
    namespace: &Namespace,
) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, namespace, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;

    // Preflight: must be Running
    if !matches!(meta.status(), RunStatus::Running { .. }) {
        anyhow::bail!("session is not running: {name}");
    }

    // Preflight: must have stdin
    if meta.launch_spec().stdin_mode != StdinMode::Pipe {
        anyhow::bail!("session was not started with --stdin: {name}");
    }

    // Acquire exec lock (single-flight per session)
    let _exec_lock = ExecLock::try_acquire(&session)?;

    // TODO: Tasks 2-5 — framing, send, tail, parse, return
    let _ = (cmd, timeout);
    anyhow::bail!("exec not yet implemented");
}
```

**Step 3: Wire into mod.rs**

Add to `src/commands/mod.rs`:

```rust
mod exec;
pub use exec::cmd_exec;
```

**Step 4: Write integration tests for preflight**

Create `tests/cli_exec.rs`:

```rust
mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// exec fails if session doesn't exist.
#[test]
fn exec_session_not_found() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["exec", "nonexistent", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session not found"));
}

/// exec fails if session is not running (terminal state).
#[test]
fn exec_session_not_running() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start a short-lived session without --stdin
    harness::tender(&root)
        .args(["start", "job1", "--", "true"])
        .assert()
        .success();
    harness::wait_terminal(&root, "job1");

    harness::tender(&root)
        .args(["exec", "job1", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not running"));
}

/// exec fails if session lacks --stdin.
#[test]
fn exec_session_no_stdin() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start a long-running session WITHOUT --stdin
    harness::tender(&root)
        .args(["start", "job1", "--", "sleep", "30"])
        .assert()
        .success();
    harness::wait_running(&root, "job1");

    harness::tender(&root)
        .args(["exec", "job1", "--", "pwd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--stdin"));

    let _ = harness::tender(&root).args(["kill", "job1", "--force"]).assert();
}
```

**Step 5: Run tests**

Run: `cargo test --test cli_exec`
Expected: All 3 preflight tests pass.

**Step 6: Commit**

```
feat(exec): add CLI command with preflight checks and exec lock
```

---

### Task 2: Sentinel framing builders

**Files:**
- Create: `src/exec_frame.rs` — pure functions for sentinel framing
- Modify: `src/lib.rs` — add `pub mod exec_frame;`

**Step 1: Write unit tests for framing**

```rust
// In src/exec_frame.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_frame_simple_command() {
        let frame = unix_frame(&["echo", "hello"], "tok123");
        assert!(frame.contains("echo hello"));
        assert!(frame.contains("__TENDER_EXEC__ tok123"));
        assert!(frame.contains("$?"));
        assert!(frame.ends_with('\n'));
    }

    #[test]
    fn unix_frame_command_with_special_chars() {
        let frame = unix_frame(&["echo", "it's a \"test\""], "tok123");
        // Should be properly shell-escaped
        assert!(frame.contains("__TENDER_EXEC__ tok123"));
    }

    #[test]
    fn parse_sentinel_valid() {
        let result =
            parse_sentinel("__TENDER_EXEC__ tok123 0 /home/user", "tok123");
        assert!(result.is_some());
        let (exit_code, cwd) = result.unwrap();
        assert_eq!(exit_code, 0);
        assert_eq!(cwd, "/home/user");
    }

    #[test]
    fn parse_sentinel_nonzero_exit() {
        let result =
            parse_sentinel("__TENDER_EXEC__ tok123 42 /tmp", "tok123");
        let (exit_code, cwd) = result.unwrap();
        assert_eq!(exit_code, 42);
        assert_eq!(cwd, "/tmp");
    }

    #[test]
    fn parse_sentinel_cwd_with_spaces() {
        let result = parse_sentinel(
            "__TENDER_EXEC__ tok123 0 /home/user/my project",
            "tok123",
        );
        let (_, cwd) = result.unwrap();
        assert_eq!(cwd, "/home/user/my project");
    }

    #[test]
    fn parse_sentinel_wrong_token() {
        let result =
            parse_sentinel("__TENDER_EXEC__ other 0 /home", "tok123");
        assert!(result.is_none());
    }

    #[test]
    fn parse_sentinel_not_sentinel() {
        let result = parse_sentinel("hello world", "tok123");
        assert!(result.is_none());
    }
}
```

**Step 2: Implement framing functions**

```rust
/// Build a framed shell command string for Unix shells (bash/sh).
///
/// Format:
/// ```sh
/// <escaped command>; __tender_s=$?; printf '__TENDER_EXEC__ %s %s %s\n' "<token>" "$__tender_s" "$(pwd)"
/// ```
pub fn unix_frame(argv: &[str], token: &str) -> String {
    let cmd = shell_words::join(argv);
    format!(
        "{cmd}; __tender_s=$?; printf '__TENDER_EXEC__ %s %s %s\\n' '{token}' \"$__tender_s\" \"$(pwd)\"\n"
    )
}

/// Build a framed command string for PowerShell.
pub fn powershell_frame(argv: &[str], token: &str) -> String {
    let cmd = argv.join(" ");
    format!(
        "{cmd}; $__tender_s=$LASTEXITCODE; if ($null -eq $__tender_s) {{ $__tender_s=0 }}; Write-Output \"__TENDER_EXEC__ {token} $__tender_s $(Get-Location)\"\n"
    )
}

/// Parse a sentinel line, extracting exit code and cwd.
/// Returns None if the line is not a sentinel or token doesn't match.
pub fn parse_sentinel(line: &str, expected_token: &str) -> Option<(i32, String)> {
    let rest = line.strip_prefix("__TENDER_EXEC__ ")?;
    let (token, rest) = rest.split_once(' ')?;
    if token != expected_token {
        return None;
    }
    let (code_str, cwd) = rest.split_once(' ')?;
    let code: i32 = code_str.parse().ok()?;
    Some((code, cwd.to_owned()))
}

/// Generate a unique token for sentinel matching.
pub fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // pid + nanos gives sufficient uniqueness for single-flight exec
    format!("{:x}{:x}", std::process::id(), nanos)
}
```

**Step 3: Run unit tests**

Run: `cargo test exec_frame`
Expected: All pass.

**Step 4: Commit**

```
feat(exec): add sentinel framing builders with unit tests
```

---

### Task 3: Log cursor + tail-until-sentinel + output parsing

**Files:**
- Modify: `src/commands/exec.rs` — add core exec logic

**Step 1: Implement the exec engine**

Add to `src/commands/exec.rs`:

```rust
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use tender::exec_frame;
use tender::log::LogLine;
use tender::platform::{Current, Platform};

/// Result of an exec command.
#[derive(serde::Serialize)]
struct ExecResult {
    session: String,
    stdout: String,
    stderr: String,
    exit_code: i32,
    cwd_after: String,
    timed_out: bool,
    truncated: bool,
}

/// Execute a command in a running shell session.
fn run_exec(
    session: &SessionDir,
    meta: &tender::model::meta::Meta,
    cmd: &[String],
    timeout: Option<u64>,
) -> anyhow::Result<ExecResult> {
    let token = exec_frame::generate_token();
    let log_path = session.path().join("output.log");

    // 1. Capture log cursor (current end of file)
    let cursor = if log_path.exists() {
        std::fs::metadata(&log_path)?.len()
    } else {
        0
    };

    // 2. Frame the command
    let frame = exec_frame::unix_frame(cmd, &token);

    // 3. Send through stdin transport
    {
        let mut writer = Current::open_stdin_writer(session.path())
            .map_err(|e| anyhow::anyhow!("failed to open stdin transport: {e}"))?;
        writer.write_all(frame.as_bytes())?;
    } // writer dropped — EOF closes this push

    // 4. Tail output.log from cursor until sentinel
    let deadline = timeout.map(|t| {
        std::time::Instant::now() + std::time::Duration::from_secs(t)
    });

    let mut stdout_lines: Vec<String> = Vec::new();
    let mut stderr_lines: Vec<String> = Vec::new();

    // Wait for log file to exist (may not exist yet if session just started)
    while !log_path.exists() {
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                return Ok(ExecResult {
                    session: meta.session().as_str().to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    cwd_after: String::new(),
                    timed_out: true,
                    truncated: false,
                });
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let file = std::fs::File::open(&log_path)?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(cursor))?;

    let mut buf = String::new();
    loop {
        buf.clear();
        let bytes = reader.read_line(&mut buf)?;
        if bytes == 0 {
            // No new data — check timeout, then sleep
            if let Some(dl) = deadline {
                if std::time::Instant::now() >= dl {
                    return Ok(ExecResult {
                        session: meta.session().as_str().to_string(),
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code: -1,
                        cwd_after: String::new(),
                        timed_out: true,
                        truncated: false,
                    });
                }
            }
            // Check session is still alive
            if let Ok(current) = session::read_meta(session) {
                if current.status().is_terminal() {
                    anyhow::bail!("session exited while waiting for exec result");
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
        let Some(parsed) = LogLine::parse(trimmed) else {
            continue;
        };

        match parsed.tag {
            'O' => {
                // Check for sentinel
                if let Some((exit_code, cwd)) =
                    exec_frame::parse_sentinel(&parsed.content, &token)
                {
                    return Ok(ExecResult {
                        session: meta.session().as_str().to_string(),
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code,
                        cwd_after: cwd,
                        timed_out: false,
                        truncated: false,
                    });
                }
                stdout_lines.push(parsed.content);
            }
            'E' => {
                stderr_lines.push(parsed.content);
            }
            _ => {} // skip annotations
        }
    }
}
```

**Step 2: Wire `run_exec` into `cmd_exec`**

Replace the TODO placeholder in `cmd_exec`:

```rust
// Execute the command
let result = run_exec(&session, &meta, &cmd, timeout)?;

// Print structured JSON result
let json = serde_json::to_string_pretty(&result)?;
println!("{json}");

// Exit with the command's exit code
if result.timed_out {
    eprintln!("exec timed out — command may still be running in the shell");
    std::process::exit(124);
}
if result.exit_code != 0 {
    std::process::exit(result.exit_code);
}

Ok(())
```

**Step 3: Write integration test — basic exec**

Add to `tests/cli_exec.rs`:

```rust
/// Basic exec: run pwd in a bash shell, get structured output.
#[test]
fn exec_basic_command() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    // Start a bash shell with --stdin
    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");

    // Give shell time to initialize
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Exec a command
    let output = harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "hello world"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"].as_str().unwrap().contains("hello world"));
    assert!(!result["timed_out"].as_bool().unwrap());

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}
```

**Step 4: Run tests**

Run: `cargo test --test cli_exec`
Expected: All pass (preflight tests + basic exec).

**Step 5: Commit**

```
feat(exec): implement core exec engine — frame, send, tail, parse
```

---

### Task 4: Annotation + non-zero exit + cwd persistence tests

**Files:**
- Modify: `src/commands/exec.rs` — add annotation writing
- Modify: `tests/cli_exec.rs` — more tests

**Step 1: Add annotation writing to exec**

In `cmd_exec`, after `run_exec` returns and before printing the JSON result, write the annotation. Reuse the annotation pattern from `wrap.rs`:

```rust
// Write annotation event
{
    let run_id = meta.run_id().to_string();
    let annotation = serde_json::json!({
        "source": "tender.exec",
        "event": "exec",
        "run_id": run_id,
        "data": {
            "command": cmd,
            "hook_stdout": &result.stdout,
            "hook_stderr": &result.stderr,
            "hook_exit_code": result.exit_code,
            "cwd_after": &result.cwd_after,
            "sentinel": format!("TENDER_EXEC_{}", token_for_annotation),
            "timed_out": result.timed_out,
            "truncated": result.truncated,
        }
    });
    let ts = timestamp_micros();
    let json = serde_json::to_string(&annotation)?;
    let line = format!("{ts} A {json}\n");
    let log_path = session.path().join("output.log");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    file.write_all(line.as_bytes())?;
}
```

Note: the `token` needs to be available after `run_exec`. Either return it from `run_exec` or generate it in `cmd_exec` and pass it in. Restructure so `cmd_exec` generates the token, passes it to `run_exec`, and keeps it for the annotation.

Also add the `timestamp_micros` helper (same pattern as `wrap.rs`):

```rust
fn timestamp_micros() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let micros = duration.subsec_micros();
    format!("{secs}.{micros:06}")
}
```

Note: `tender.exec` is NOT a valid `Source` (reserved prefix `tender.*`). Use a non-reserved source like `exec.result` or have the caller pass `--source` — but the spec says `"source": "agent.hooks"`. For simplicity, use `"agent.exec"` as the source string. Since annotation writing doesn't go through `Source::new()` validation (it's raw JSON), the `tender.*` reservation only applies to the `wrap` command's `--source` flag.

**Step 2: Write integration tests**

Add to `tests/cli_exec.rs`:

```rust
/// exec propagates non-zero exit code.
#[test]
fn exec_nonzero_exit() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = harness::tender(&root)
        .args(["exec", "shell", "--", "false"])
        .output()
        .unwrap();

    // exec exits with the command's exit code
    assert!(!output.status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"].as_i64(), Some(1));

    // Shell should still be running
    harness::tender(&root)
        .args(["status", "shell"])
        .assert()
        .success();

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}

/// Shell state persists across exec calls (cwd).
#[test]
fn exec_cwd_persists() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // cd to /tmp
    let output1 = harness::tender(&root)
        .args(["exec", "shell", "--", "cd", "/tmp"])
        .output()
        .unwrap();
    let result1: serde_json::Value =
        serde_json::from_slice(&output1.stdout).unwrap();
    assert_eq!(result1["cwd_after"].as_str(), Some("/tmp"));

    // Next exec should see /tmp as cwd
    let output2 = harness::tender(&root)
        .args(["exec", "shell", "--", "pwd"])
        .output()
        .unwrap();
    let result2: serde_json::Value =
        serde_json::from_slice(&output2.stdout).unwrap();
    assert!(result2["stdout"].as_str().unwrap().contains("/tmp"));
    assert_eq!(result2["cwd_after"].as_str(), Some("/tmp"));

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}

/// Annotation event is written to output.log.
#[test]
fn exec_writes_annotation() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "annotated"])
        .assert()
        .success();

    // Read output.log and check for annotation
    let log_path = root
        .path()
        .join(".tender/sessions/default/shell/output.log");
    let content = std::fs::read_to_string(&log_path).unwrap();
    // Find annotation line
    let ann_line = content
        .lines()
        .find(|l| l.contains(" A ") && l.contains("exec"))
        .expect("annotation line should exist");
    let json_start = ann_line.find('{').unwrap();
    let ann: serde_json::Value =
        serde_json::from_str(&ann_line[json_start..]).unwrap();
    assert_eq!(ann["event"].as_str(), Some("exec"));
    assert_eq!(ann["data"]["hook_exit_code"].as_i64(), Some(0));

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}
```

**Step 3: Run tests**

Run: `cargo test --test cli_exec`
Expected: All pass.

**Step 4: Commit**

```
feat(exec): add annotation events and cwd persistence
```

---

### Task 5: Timeout + exec lock concurrency test

**Files:**
- Modify: `tests/cli_exec.rs`

**Step 1: Timeout test**

```rust
/// exec --timeout: returns timeout error, shell stays alive.
#[test]
fn exec_timeout() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Run a long command with short timeout
    let output = harness::tender(&root)
        .args(["exec", "shell", "--timeout", "2", "--", "sleep", "60"])
        .output()
        .unwrap();

    // Should exit 124 (timeout)
    assert_eq!(output.status.code(), Some(124));
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["timed_out"].as_bool().unwrap());

    // Shell should still be running
    harness::tender(&root)
        .args(["status", "shell"])
        .assert()
        .success();

    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}
```

**Step 2: Concurrent exec lock test**

```rust
/// Second concurrent exec fails with busy error.
#[test]
fn exec_concurrent_busy() {
    let _lock = lock();
    let root = tempfile::TempDir::new().unwrap();

    harness::tender(&root)
        .args(["start", "shell", "--stdin", "--", "bash"])
        .assert()
        .success();
    harness::wait_running(&root, "shell");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Start a long exec in the background
    let mut long_exec = std::process::Command::new(
        assert_cmd::cargo::cargo_bin("tender"),
    )
    .env("HOME", root.path())
    .args(["exec", "shell", "--", "sleep", "30"])
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();

    // Give it time to acquire the lock
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Second exec should fail with busy
    harness::tender(&root)
        .args(["exec", "shell", "--", "echo", "hello"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("another exec"));

    // Clean up
    let _ = long_exec.kill();
    let _ = long_exec.wait();
    let _ = harness::tender(&root).args(["kill", "shell", "--force"]).assert();
}
```

**Step 3: Run all tests**

Run: `cargo test --test cli_exec`
Expected: All pass.

**Step 4: Run full suite**

Run: `cargo test`
Expected: All pass.

**Step 5: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No new warnings.

**Step 6: Commit**

```
test: add timeout and concurrency tests for exec
```

---

### Task 6: Final verification

**Step 1: Full test suite**

Run: `cargo test`
Expected: All pass.

**Step 2: Clippy**

Run: `cargo clippy -- -D warnings`
Expected: Clean.

**Step 3: Commit**

```
feat: implement tender exec — structured commands in a persistent shell
```

---

## Testing Matrix

| Test | Exercises |
|------|-----------|
| `exec_session_not_found` | Preflight: session existence |
| `exec_session_not_running` | Preflight: Running state |
| `exec_session_no_stdin` | Preflight: StdinMode::Pipe |
| `exec_basic_command` | Full flow: frame → send → tail → parse → return |
| `exec_nonzero_exit` | Exit code propagation, shell survival |
| `exec_cwd_persists` | State persistence across calls |
| `exec_writes_annotation` | Annotation in output.log |
| `exec_timeout` | Client-side timeout, exit 124, shell survives |
| `exec_concurrent_busy` | Exec lock single-flight |

## Design Decisions

**Client-side timeout only**: The shell command keeps running after timeout. Killing the in-shell command without killing the shell is follow-on work.

**Unix-first framing**: `shell_words::join` for argument escaping. PowerShell framing exists but is not integration-tested in this slice (Windows CI covers it).

**Annotation source**: Uses `"agent.exec"` — avoids the `tender.*` reserved prefix.

**Log cursor is byte offset**: Seek to end of `output.log` before sending. This is more reliable than timestamp-based filtering since the sentinel must be found after the cursor.

**Exit code propagation**: `exec` exits with the command's exit code (like `run`). Timeout exits 124 (matching existing convention).

## Depends On

`wrap-annotation-ingestion` (complete). Reuses the annotation envelope and `output.log` infrastructure.

## Not In Scope

- Starting a shell automatically if session missing
- Killing only the in-shell command while preserving the shell
- Binary-safe framing
- Concurrent queued `exec` calls
- PTY-backed shells
- Remote execution over SSH
