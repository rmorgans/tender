# Slice 6: Log Query Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `tender log <session>` with `--tail`, `--follow`, `--grep`, `--since`, `--raw` flags for querying captured child output.

**Architecture:** New `src/log.rs` module parses the existing `output.log` line format (`{secs}.{micros} {O|E} {content}`). All filtering happens in a streaming pipeline: parse line -> time filter -> grep filter -> format (raw or prefixed) -> print. `--follow` uses poll-based tailing with session liveness check (no tokio dependency). `--tail N` buffers last N lines in a ring buffer before printing.

**Tech Stack:** Rust std only (no new deps). `BufReader`, `VecDeque` for tail ring buffer, `std::thread::sleep` for follow polling.

**Log line format (from `sidecar.rs:capture_stream`):**
```
1773653954.012345 O hello world
1773653954.012400 E some error
```
Fields: `{epoch_secs}.{micros_06} {stream_tag} {content}`

---

## Task 1: Log line parser and types

**Files:**
- Create: `src/log.rs`
- Modify: `src/lib.rs` (add `pub mod log;`)

### Step 1: Write failing tests for log line parsing

Create `src/log.rs` with the test module first:

```rust
/// Parsed log line from output.log.
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    /// Epoch timestamp in microseconds (parsed from "secs.micros" format).
    pub timestamp_us: u64,
    /// Stream tag: 'O' for stdout, 'E' for stderr.
    pub tag: char,
    /// Line content (without timestamp/tag prefix, without trailing newline).
    pub content: String,
}

impl LogLine {
    /// Parse a single log line. Returns None for malformed lines.
    pub fn parse(line: &str) -> Option<Self> {
        // Format: "1773653954.012345 O hello world"
        let (ts_str, rest) = line.split_once(' ')?;
        let (tag_str, content) = rest.split_once(' ').unwrap_or((rest, ""));

        let tag = tag_str.chars().next()?;
        if tag != 'O' && tag != 'E' {
            return None;
        }

        let (secs_str, micros_str) = ts_str.split_once('.')?;
        let secs: u64 = secs_str.parse().ok()?;
        let micros: u64 = micros_str.parse().ok()?;
        let timestamp_us = secs * 1_000_000 + micros;

        Some(LogLine {
            timestamp_us,
            tag,
            content: content.to_owned(),
        })
    }

    /// Format as the original log line (with prefix).
    pub fn format_prefixed(&self) -> String {
        let secs = self.timestamp_us / 1_000_000;
        let micros = self.timestamp_us % 1_000_000;
        format!("{secs}.{micros:06} {} {}", self.tag, self.content)
    }

    /// Format as raw content (no prefix).
    pub fn format_raw(&self) -> &str {
        &self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdout_line() {
        let line = "1773653954.012345 O hello world";
        let parsed = LogLine::parse(line).unwrap();
        assert_eq!(parsed.timestamp_us, 1773653954_012345);
        assert_eq!(parsed.tag, 'O');
        assert_eq!(parsed.content, "hello world");
    }

    #[test]
    fn parse_stderr_line() {
        let line = "1773653954.000100 E some error";
        let parsed = LogLine::parse(line).unwrap();
        assert_eq!(parsed.tag, 'E');
        assert_eq!(parsed.content, "some error");
    }

    #[test]
    fn parse_empty_content() {
        let line = "1773653954.000000 O ";
        let parsed = LogLine::parse(line).unwrap();
        assert_eq!(parsed.content, "");
    }

    #[test]
    fn parse_content_with_spaces() {
        let line = "1773653954.000000 O hello  world  foo";
        let parsed = LogLine::parse(line).unwrap();
        assert_eq!(parsed.content, "hello  world  foo");
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert!(LogLine::parse("garbage").is_none());
        assert!(LogLine::parse("").is_none());
        assert!(LogLine::parse("123.456 X data").is_none()); // bad tag
    }

    #[test]
    fn format_prefixed_roundtrip() {
        let line = "1773653954.012345 O hello world";
        let parsed = LogLine::parse(line).unwrap();
        assert_eq!(parsed.format_prefixed(), line);
    }

    #[test]
    fn format_raw_strips_prefix() {
        let line = "1773653954.012345 O hello world";
        let parsed = LogLine::parse(line).unwrap();
        assert_eq!(parsed.format_raw(), "hello world");
    }
}
```

### Step 2: Run tests to verify they pass

```bash
cargo test --lib -- log::tests -v
```

Expected: 7 tests PASS (code and tests are in the same step since the types are trivial).

### Step 3: Add module to lib.rs

Add `pub mod log;` to `src/lib.rs`.

### Step 4: Commit

```bash
git add src/log.rs src/lib.rs
git commit -m "feat(log): add LogLine parser and format methods"
```

---

## Task 2: Log query engine (read, filter, tail)

**Files:**
- Modify: `src/log.rs`

### Step 1: Add query parameters struct and read_log function

Add to `src/log.rs` above the test module:

```rust
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

/// Parameters for log queries.
pub struct LogQuery {
    /// Only show last N lines (applied after all other filters).
    pub tail: Option<usize>,
    /// Only show lines matching this substring.
    pub grep: Option<String>,
    /// Only show lines at or after this epoch timestamp (microseconds).
    pub since_us: Option<u64>,
    /// Strip prefix (timestamp + tag), print content only.
    pub raw: bool,
}

impl Default for LogQuery {
    fn default() -> Self {
        Self {
            tail: None,
            grep: None,
            since_us: None,
            raw: false,
        }
    }
}

/// Read and query a log file, writing matching lines to the writer.
/// Returns the number of lines written.
pub fn query_log(path: &Path, query: &LogQuery, out: &mut dyn Write) -> io::Result<usize> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut lines: Vec<LogLine> = Vec::new();

    for raw_line in reader.lines() {
        let raw_line = raw_line?;
        let Some(parsed) = LogLine::parse(&raw_line) else {
            continue; // skip malformed lines
        };

        // Time filter
        if let Some(since_us) = query.since_us {
            if parsed.timestamp_us < since_us {
                continue;
            }
        }

        // Grep filter
        if let Some(ref pattern) = query.grep {
            if !parsed.content.contains(pattern.as_str()) {
                continue;
            }
        }

        lines.push(parsed);
    }

    // Tail: keep only last N
    let start = if let Some(n) = query.tail {
        lines.len().saturating_sub(n)
    } else {
        0
    };

    let mut count = 0;
    for line in &lines[start..] {
        if query.raw {
            writeln!(out, "{}", line.format_raw())?;
        } else {
            writeln!(out, "{}", line.format_prefixed())?;
        }
        count += 1;
    }

    Ok(count)
}
```

### Step 2: Write tests for query_log

Add to the test module:

```rust
use std::io::Write as _;

fn write_test_log(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("output.log");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "1000000.000000 O line one").unwrap();
    writeln!(f, "1000001.000000 O line two").unwrap();
    writeln!(f, "1000002.000000 E error here").unwrap();
    writeln!(f, "1000003.000000 O line four").unwrap();
    writeln!(f, "1000004.000000 O line five with error word").unwrap();
    path
}

#[test]
fn query_full_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_log(dir.path());
    let mut buf = Vec::new();
    let count = query_log(&path, &LogQuery::default(), &mut buf).unwrap();
    assert_eq!(count, 5);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("line one"));
    assert!(output.contains("line five"));
}

#[test]
fn query_tail_2() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_log(dir.path());
    let mut buf = Vec::new();
    let query = LogQuery { tail: Some(2), ..Default::default() };
    let count = query_log(&path, &query, &mut buf).unwrap();
    assert_eq!(count, 2);
    let output = String::from_utf8(buf).unwrap();
    assert!(!output.contains("line one"));
    assert!(output.contains("line four"));
    assert!(output.contains("line five"));
}

#[test]
fn query_grep() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_log(dir.path());
    let mut buf = Vec::new();
    let query = LogQuery { grep: Some("error".into()), ..Default::default() };
    let count = query_log(&path, &query, &mut buf).unwrap();
    assert_eq!(count, 2); // "error here" and "line five with error word"
}

#[test]
fn query_since() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_log(dir.path());
    let mut buf = Vec::new();
    let query = LogQuery { since_us: Some(1000003_000000), ..Default::default() };
    let count = query_log(&path, &query, &mut buf).unwrap();
    assert_eq!(count, 2); // line four + line five
}

#[test]
fn query_raw() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_log(dir.path());
    let mut buf = Vec::new();
    let query = LogQuery { raw: true, tail: Some(1), ..Default::default() };
    query_log(&path, &query, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert_eq!(output.trim(), "line five with error word");
    assert!(!output.contains("1000004")); // no timestamp
}

#[test]
fn query_combined_grep_and_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_log(dir.path());
    let mut buf = Vec::new();
    let query = LogQuery {
        grep: Some("error".into()),
        tail: Some(1),
        ..Default::default()
    };
    let count = query_log(&path, &query, &mut buf).unwrap();
    assert_eq!(count, 1);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("line five with error word"));
}

#[test]
fn query_missing_file_returns_error() {
    let mut buf = Vec::new();
    let result = query_log(Path::new("/nonexistent/output.log"), &LogQuery::default(), &mut buf);
    assert!(result.is_err());
}
```

### Step 3: Run tests

```bash
cargo test --lib -- log::tests -v
```

Expected: 14 tests PASS.

### Step 4: Commit

```bash
git add src/log.rs
git commit -m "feat(log): add query_log with tail/grep/since/raw filtering"
```

---

## Task 3: Follow mode

**Files:**
- Modify: `src/log.rs`

### Step 1: Add follow_log function

`--follow` streams lines as they're written. It stops when the session reaches a terminal state (checked via meta.json). Uses poll-based tailing — seek to end, read new lines, sleep, repeat.

Add to `src/log.rs`:

```rust
use std::io::{Seek, SeekFrom};

/// Follow a log file, printing new lines as they appear.
/// Stops when `should_stop` returns true (session reached terminal state).
/// Applies grep and raw filters but not tail or since (follow is live-only).
pub fn follow_log<F>(
    path: &Path,
    query: &LogQuery,
    out: &mut dyn Write,
    should_stop: F,
) -> io::Result<()>
where
    F: Fn() -> bool,
{
    // Wait for file to exist (session may still be in Starting state)
    let file = loop {
        match std::fs::File::open(path) {
            Ok(f) => break f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if should_stop() {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    };

    let mut reader = BufReader::new(file);

    // If not tailing from start, seek to end
    if query.tail.is_none() && query.since_us.is_none() {
        reader.seek(SeekFrom::End(0))?;
    }

    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_line(&mut line_buf)?;

        if bytes_read == 0 {
            // No new data — check if session is terminal
            if should_stop() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        let trimmed = line_buf.trim_end_matches('\n');
        let Some(parsed) = LogLine::parse(trimmed) else {
            continue;
        };

        // Time filter
        if let Some(since_us) = query.since_us {
            if parsed.timestamp_us < since_us {
                continue;
            }
        }

        // Grep filter
        if let Some(ref pattern) = query.grep {
            if !parsed.content.contains(pattern.as_str()) {
                continue;
            }
        }

        if query.raw {
            writeln!(out, "{}", parsed.format_raw())?;
        } else {
            writeln!(out, "{}", parsed.format_prefixed())?;
        }
        out.flush()?;
    }
}
```

### Step 2: Write test for follow mode

Add to test module:

```rust
#[test]
fn follow_stops_on_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("output.log");

    // Write initial content
    {
        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "1000000.000000 O initial line").unwrap();
    }

    // Spawn follow in a thread, stop immediately
    let path = log_path.clone();
    let mut buf = Vec::new();
    let query = LogQuery { ..Default::default() };
    // should_stop returns true immediately — follow reads what's there and exits
    follow_log(&path, &query, &mut buf, || true).unwrap();

    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("initial line"));
}

#[test]
fn follow_picks_up_new_lines() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("output.log");
    std::fs::File::create(&log_path).unwrap(); // empty file

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let path = log_path.clone();

    let handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let query = LogQuery { ..Default::default() };
        follow_log(&path, &query, &mut buf, || stop_clone.load(Ordering::Relaxed)).unwrap();
        String::from_utf8(buf).unwrap()
    });

    // Give follow thread time to start
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Append a line
    {
        use std::fs::OpenOptions;
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(f, "1000001.000000 O appended line").unwrap();
    }

    // Let follow pick it up
    std::thread::sleep(std::time::Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);

    let output = handle.join().unwrap();
    assert!(output.contains("appended line"));
}

#[test]
fn follow_waits_for_file_creation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("output.log");
    // File does NOT exist yet

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let path = log_path.clone();

    let handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let query = LogQuery { ..Default::default() };
        follow_log(&path, &query, &mut buf, || stop_clone.load(Ordering::Relaxed)).unwrap();
        String::from_utf8(buf).unwrap()
    });

    // Create file after a delay
    std::thread::sleep(std::time::Duration::from_millis(200));
    {
        let mut f = std::fs::File::create(&log_path).unwrap();
        writeln!(f, "1000000.000000 O late arrival").unwrap();
    }

    std::thread::sleep(std::time::Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);

    let output = handle.join().unwrap();
    assert!(output.contains("late arrival"));
}
```

### Step 3: Run tests

```bash
cargo test --lib -- log::tests -v
```

Expected: 17 tests PASS.

### Step 4: Commit

```bash
git add src/log.rs
git commit -m "feat(log): add follow_log with poll-based tailing and terminal stop"
```

---

## Task 4: Duration parser for --since

**Files:**
- Modify: `src/log.rs`

### Step 1: Add parse_since and its tests

`--since` accepts either epoch seconds (`1773653954`) or a human duration (`5m`, `2h`, `30s`). Returns epoch microseconds.

Add to `src/log.rs`:

```rust
/// Parse a --since value into epoch microseconds.
/// Accepts:
/// - Epoch seconds: "1773653954" -> that time in microseconds
/// - Duration suffix: "30s", "5m", "2h", "1d" -> now minus that duration
pub fn parse_since(value: &str) -> Result<u64, String> {
    // Try as a duration with suffix
    if let Some(num_str) = value.strip_suffix('s') {
        let n: u64 = num_str.parse().map_err(|_| format!("invalid duration: {value}"))?;
        return Ok(now_us().saturating_sub(n * 1_000_000));
    }
    if let Some(num_str) = value.strip_suffix('m') {
        let n: u64 = num_str.parse().map_err(|_| format!("invalid duration: {value}"))?;
        return Ok(now_us().saturating_sub(n * 60 * 1_000_000));
    }
    if let Some(num_str) = value.strip_suffix('h') {
        let n: u64 = num_str.parse().map_err(|_| format!("invalid duration: {value}"))?;
        return Ok(now_us().saturating_sub(n * 3600 * 1_000_000));
    }
    if let Some(num_str) = value.strip_suffix('d') {
        let n: u64 = num_str.parse().map_err(|_| format!("invalid duration: {value}"))?;
        return Ok(now_us().saturating_sub(n * 86400 * 1_000_000));
    }

    // Try as raw epoch seconds
    let secs: u64 = value
        .parse()
        .map_err(|_| format!("invalid --since value: {value} (expected epoch seconds or duration like 5m, 2h)"))?;
    Ok(secs * 1_000_000)
}

fn now_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
```

Tests:

```rust
#[test]
fn parse_since_epoch_seconds() {
    let us = parse_since("1000000").unwrap();
    assert_eq!(us, 1000000_000000);
}

#[test]
fn parse_since_seconds_duration() {
    let before = now_us();
    let us = parse_since("30s").unwrap();
    let after = now_us();
    // Should be approximately now - 30s
    assert!(us >= before - 31_000_000);
    assert!(us <= after - 29_000_000);
}

#[test]
fn parse_since_minutes_duration() {
    let us = parse_since("5m").unwrap();
    let now = now_us();
    let diff = now - us;
    // Should be ~5 minutes (300 seconds) in microseconds
    assert!(diff >= 299_000_000 && diff <= 301_000_000);
}

#[test]
fn parse_since_invalid() {
    assert!(parse_since("abc").is_err());
    assert!(parse_since("5x").is_err());
    assert!(parse_since("").is_err());
}
```

### Step 2: Run tests

```bash
cargo test --lib -- log::tests -v
```

Expected: 21 tests PASS.

### Step 3: Commit

```bash
git add src/log.rs
git commit -m "feat(log): add parse_since for epoch seconds and duration suffixes"
```

---

## Task 5: Wire up CLI subcommand

**Files:**
- Modify: `src/main.rs`

### Step 1: Add Log subcommand to clap

Add to the `Commands` enum:

```rust
/// Query session output log
Log {
    /// Session name
    name: String,
    /// Show last N lines
    #[arg(short = 'n', long)]
    tail: Option<usize>,
    /// Follow log output (like tail -f)
    #[arg(short, long)]
    follow: bool,
    /// Filter lines containing PATTERN
    #[arg(short, long)]
    grep: Option<String>,
    /// Show lines since TIME (epoch seconds or duration: 30s, 5m, 2h, 1d)
    #[arg(short, long)]
    since: Option<String>,
    /// Strip timestamp and stream tag prefixes
    #[arg(short, long)]
    raw: bool,
},
```

Add the match arm in `main()`:

```rust
Commands::Log { name, tail, follow, grep, since, raw } => {
    cmd_log(&name, tail, follow, grep, since, raw)
}
```

### Step 2: Implement cmd_log

```rust
fn cmd_log(
    name: &str,
    tail: Option<usize>,
    follow: bool,
    grep: Option<String>,
    since: Option<String>,
    raw: bool,
) -> anyhow::Result<()> {
    use tender::log::{self, LogQuery};
    use tender::model::ids::SessionName;
    use tender::session::{self, SessionRoot};

    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let log_path = session.path().join("output.log");

    let since_us = match since {
        Some(ref s) => Some(log::parse_since(s).map_err(|e| anyhow::anyhow!(e))?),
        None => None,
    };

    let query = LogQuery {
        tail,
        grep,
        since_us,
        raw,
    };

    let mut stdout = std::io::stdout().lock();

    if follow {
        let session_ref = &session;
        log::follow_log(&log_path, &query, &mut stdout, || {
            session::read_meta(session_ref)
                .map(|m| m.status().is_terminal())
                .unwrap_or(false)
        })?;
    } else {
        if !log_path.exists() {
            // No log file — session may be Starting or SpawnFailed
            // Not an error, just nothing to show
            return Ok(());
        }
        log::query_log(&log_path, &query, &mut stdout)?;
    }

    Ok(())
}
```

### Step 3: Verify it compiles

```bash
cargo build
```

### Step 4: Commit

```bash
git add src/main.rs
git commit -m "feat(log): wire up tender log subcommand with all flags"
```

---

## Task 6: Integration tests

**Files:**
- Create: `tests/cli_log.rs`

### Step 1: Write integration tests

```rust
use std::process::Command;
use tempfile::TempDir;

fn tender_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_tender"))
}

fn run_tender(root: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(tender_bin())
        .args(args)
        .env("HOME", root.path())
        .output()
        .expect("failed to run tender")
}

fn wait_terminal(root: &TempDir, session: &str) {
    let path = root
        .path()
        .join(format!(".tender/sessions/{session}/meta.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                let status = meta["status"].as_str().unwrap_or("");
                if status != "Starting" && status != "Running" {
                    return;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for terminal state in {session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn log_shows_child_output() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "log-test", "echo", "hello from child"]);
    wait_terminal(&root, "log-test");

    let output = run_tender(&root, &["log", "log-test"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello from child"), "stdout: {stdout}");
}

#[test]
fn log_tail() {
    let root = TempDir::new().unwrap();
    // Use printf to emit multiple lines (sh -c for portability)
    run_tender(&root, &["start", "tail-test", "sh", "-c", "echo line1; echo line2; echo line3"]);
    wait_terminal(&root, "tail-test");

    let output = run_tender(&root, &["log", "--tail", "1", "tail-test"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("line3"));
    assert!(!stdout.contains("line1"));
}

#[test]
fn log_grep() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "grep-test", "sh", "-c", "echo good; echo bad; echo good again"]);
    wait_terminal(&root, "grep-test");

    let output = run_tender(&root, &["log", "--grep", "good", "grep-test"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("good"));
    assert!(!stdout.contains("bad"));
}

#[test]
fn log_raw_strips_prefix() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "raw-test", "echo", "just content"]);
    wait_terminal(&root, "raw-test");

    let output = run_tender(&root, &["log", "--raw", "raw-test"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "just content");
    // Should not contain timestamp digits at start of line
    assert!(!stdout.starts_with(char::is_numeric));
}

#[test]
fn log_nonexistent_session_fails() {
    let root = TempDir::new().unwrap();
    let output = run_tender(&root, &["log", "nope"]);
    assert!(!output.status.success());
}

#[test]
fn log_no_output_file_returns_empty() {
    let root = TempDir::new().unwrap();
    // SpawnFailed has no output.log
    run_tender(&root, &["start", "nolog-test", "/nonexistent/binary"]);
    wait_terminal(&root, "nolog-test");

    let output = run_tender(&root, &["log", "nolog-test"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.is_empty() || stdout.trim().is_empty());
}

#[test]
fn log_stderr_captured() {
    let root = TempDir::new().unwrap();
    run_tender(&root, &["start", "stderr-test", "sh", "-c", "echo err >&2"]);
    wait_terminal(&root, "stderr-test");

    let output = run_tender(&root, &["log", "stderr-test"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("err"), "stderr should be in log: {stdout}");
    assert!(stdout.contains(" E "), "stderr lines should have E tag: {stdout}");
}
```

### Step 2: Run integration tests

```bash
cargo test --test cli_log -v
```

Expected: 7 tests PASS.

### Step 3: Commit

```bash
git add tests/cli_log.rs
git commit -m "test(log): add integration tests for log subcommand"
```

---

## Task 7: Full suite verification

### Step 1: Run all tests

```bash
cargo test
```

Expected: 106+ tests, 0 failures.

### Step 2: Run clippy

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: Clean.

### Step 3: Run fmt check

```bash
cargo fmt --check
```

Expected: Clean.

### Step 4: Final commit (if any formatting fixes needed)

```bash
cargo fmt
git add -A
git commit -m "style: format"
```

---

## Summary

| Task | What | Tests Added |
|------|------|-------------|
| 1 | LogLine parser + format | 7 unit |
| 2 | query_log with filters | 7 unit |
| 3 | follow_log with polling | 3 unit |
| 4 | parse_since duration parser | 4 unit |
| 5 | CLI wiring (cmd_log) | 0 (compile check) |
| 6 | Integration tests | 7 integration |
| 7 | Full suite + clippy + fmt | 0 (verification) |

**Total new tests:** ~28
**New files:** `src/log.rs`, `tests/cli_log.rs`
**Modified files:** `src/lib.rs`, `src/main.rs`
**No new dependencies.**
