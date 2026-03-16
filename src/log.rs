use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Query parameters for filtering log output.
#[derive(Default)]
pub struct LogQuery {
    pub tail: Option<usize>,
    pub grep: Option<String>,
    pub since_us: Option<u64>,
    pub raw: bool,
}

/// Read and filter a log file, writing matching lines to `out`.
///
/// Returns the number of lines written. Malformed lines are silently skipped.
pub fn query_log(path: &Path, query: &LogQuery, out: &mut dyn Write) -> io::Result<usize> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    // When tail is set, use a ring buffer capped at N to avoid unbounded
    // memory growth on long-running sessions. Without tail, stream directly.
    let tail_n = query.tail;
    let mut ring: VecDeque<LogLine> = VecDeque::with_capacity(tail_n.unwrap_or(0));
    let mut count = 0usize;

    for raw_line in reader.lines() {
        let raw_line = raw_line?;
        let Some(parsed) = LogLine::parse(&raw_line) else {
            continue;
        };

        if !matches_query(&parsed, query) {
            continue;
        }

        if let Some(n) = tail_n {
            if n > 0 {
                if ring.len() == n {
                    ring.pop_front();
                }
                ring.push_back(parsed);
            }
        } else {
            write_line(out, &parsed, query.raw)?;
            count += 1;
        }
    }

    // Flush the ring buffer (tail mode)
    for line in &ring {
        write_line(out, line, query.raw)?;
        count += 1;
    }

    Ok(count)
}

/// Check if a parsed log line passes the query filters (since + grep).
fn matches_query(line: &LogLine, query: &LogQuery) -> bool {
    if let Some(threshold) = query.since_us {
        if line.timestamp_us < threshold {
            return false;
        }
    }
    if let Some(ref pattern) = query.grep {
        if !line.content.contains(pattern.as_str()) {
            return false;
        }
    }
    true
}

fn write_line(out: &mut dyn Write, line: &LogLine, raw: bool) -> io::Result<()> {
    if raw {
        writeln!(out, "{}", line.format_raw())
    } else {
        writeln!(out, "{}", line.format_prefixed())
    }
}

/// Follow a log file, writing new lines as they appear.
///
/// Waits for the file to exist, then tails it. Applies grep/since/raw
/// filters from the query. If neither `tail` nor `since_us` is set,
/// seeks to end of file (only showing new lines). Polls every 100ms
/// and returns `Ok(())` when `should_stop` returns true.
pub fn follow_log<F>(
    path: &Path,
    query: &LogQuery,
    out: &mut dyn Write,
    should_stop: F,
) -> io::Result<()>
where
    F: Fn() -> bool,
{
    // Wait for file to exist.
    while !path.exists() {
        if should_stop() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    // If no tail/since constraint, seek to end (only show new lines).
    if query.tail.is_none() && query.since_us.is_none() {
        reader.seek(SeekFrom::End(0))?;
    }

    // If tail is set, read existing lines into a ring buffer (O(N) memory),
    // apply filters, flush the last N, then enter the live loop.
    if let Some(n) = query.tail {
        let mut ring: VecDeque<LogLine> = VecDeque::with_capacity(n);
        let mut buf = String::new();
        loop {
            buf.clear();
            let bytes = reader.read_line(&mut buf)?;
            if bytes == 0 {
                break;
            }
            let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
            let Some(parsed) = LogLine::parse(trimmed) else {
                continue;
            };
            if !matches_query(&parsed, query) {
                continue;
            }
            if n > 0 {
                if ring.len() == n {
                    ring.pop_front();
                }
                ring.push_back(parsed);
            }
        }
        for line in &ring {
            write_line(out, line, query.raw)?;
            out.flush()?;
        }
    }

    // Live tail loop.
    let mut buf = String::new();
    loop {
        buf.clear();
        let bytes = reader.read_line(&mut buf)?;
        if bytes == 0 {
            if should_stop() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        let trimmed = buf.trim_end_matches('\n').trim_end_matches('\r');
        let Some(parsed) = LogLine::parse(trimmed) else {
            continue;
        };

        if !matches_query(&parsed, query) {
            continue;
        }

        write_line(out, &parsed, query.raw)?;
        out.flush()?;
    }
}

/// Return current time in microseconds since Unix epoch.
fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_micros() as u64
}

/// Parse a "since" value into a microsecond epoch threshold.
///
/// Accepts:
/// - Duration suffixes: `"30s"`, `"5m"`, `"2h"`, `"1d"` (relative to now)
/// - Plain epoch seconds: `"1000000"` (converted to microseconds)
pub fn parse_since(value: &str) -> Result<u64, String> {
    if value.is_empty() {
        return Err("empty value".to_owned());
    }

    // Check for duration suffix.
    let last = value.as_bytes()[value.len() - 1];
    let multiplier = match last {
        b's' => Some(1_000_000u64),
        b'm' => Some(60_000_000u64),
        b'h' => Some(3_600_000_000u64),
        b'd' => Some(86_400_000_000u64),
        _ => None,
    };

    if let Some(mult) = multiplier {
        let num_str = &value[..value.len() - 1];
        let n: u64 = num_str
            .parse()
            .map_err(|_| format!("invalid duration number: {num_str:?}"))?;
        let duration_us = n
            .checked_mul(mult)
            .ok_or_else(|| format!("duration overflow: {value:?}"))?;
        return Ok(now_us().saturating_sub(duration_us));
    }

    // Plain epoch seconds.
    let secs: u64 = value
        .parse()
        .map_err(|_| format!("invalid since value: {value:?}"))?;
    secs.checked_mul(1_000_000)
        .ok_or_else(|| format!("epoch overflow: {value:?}"))
}

/// Parsed representation of a single sidecar log line.
///
/// The on-disk format produced by `sidecar::capture_stream` is:
/// ```text
/// {epoch_secs}.{micros:06} {O|E} {content}\n
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Microseconds since Unix epoch.
    pub timestamp_us: u64,
    /// `'O'` for stdout, `'E'` for stderr.
    pub tag: char,
    /// The captured line content (may be empty).
    pub content: String,
}

impl LogLine {
    /// Parse a single log line in sidecar format.
    ///
    /// Returns `None` if the line is malformed: wrong structure, invalid
    /// numbers, or a tag that is neither `O` nor `E`.
    pub fn parse(line: &str) -> Option<Self> {
        // Split off the timestamp portion: "{secs}.{micros:06}"
        let (timestamp_str, rest) = line.split_once(' ')?;
        let (secs_str, micros_str) = timestamp_str.split_once('.')?;

        let secs: u64 = secs_str.parse().ok()?;
        let micros: u64 = micros_str.parse().ok()?;

        if micros_str.len() != 6 {
            return None;
        }

        // Next character must be the tag, followed by a space.
        let tag = rest.as_bytes().first().copied()? as char;
        if tag != 'O' && tag != 'E' {
            return None;
        }

        // After the tag there must be a space separator.
        if rest.as_bytes().get(1).copied()? != b' ' {
            return None;
        }

        let content = &rest[2..];

        Some(LogLine {
            timestamp_us: secs.checked_mul(1_000_000)?.checked_add(micros)?,
            tag,
            content: content.to_owned(),
        })
    }

    /// Reconstruct the original sidecar log format.
    pub fn format_prefixed(&self) -> String {
        let secs = self.timestamp_us / 1_000_000;
        let micros = self.timestamp_us % 1_000_000;
        format!("{secs}.{micros:06} {} {}", self.tag, self.content)
    }

    /// Return just the line content, without timestamp or tag prefix.
    pub fn format_raw(&self) -> &str {
        &self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn parse_stdout_line() {
        let line = "1773653954.012345 O hello world";
        let parsed = LogLine::parse(line).expect("should parse");
        assert_eq!(parsed.timestamp_us, 1_773_653_954_012_345);
        assert_eq!(parsed.tag, 'O');
        assert_eq!(parsed.content, "hello world");
    }

    #[test]
    fn parse_stderr_line() {
        let line = "1773653954.012345 E some error";
        let parsed = LogLine::parse(line).expect("should parse");
        assert_eq!(parsed.tag, 'E');
        assert_eq!(parsed.content, "some error");
    }

    #[test]
    fn parse_empty_content() {
        let line = "1773653954.000000 O ";
        let parsed = LogLine::parse(line).expect("should parse");
        assert_eq!(parsed.timestamp_us, 1_773_653_954_000_000);
        assert_eq!(parsed.tag, 'O');
        assert_eq!(parsed.content, "");
    }

    #[test]
    fn parse_content_with_spaces() {
        let line = "1773653954.012345 O   hello   world  ";
        let parsed = LogLine::parse(line).expect("should parse");
        assert_eq!(parsed.content, "  hello   world  ");
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert!(LogLine::parse("").is_none(), "empty string");
        assert!(LogLine::parse("garbage").is_none(), "no structure");
        assert!(
            LogLine::parse("1773653954.012345 X hello").is_none(),
            "bad tag X"
        );
        assert!(
            LogLine::parse("notanumber.012345 O hello").is_none(),
            "non-numeric secs"
        );
        assert!(
            LogLine::parse("1773653954.12 O hello").is_none(),
            "short micros field"
        );
        assert!(
            LogLine::parse("1773653954.012345 O").is_none(),
            "missing space after tag"
        );
    }

    #[test]
    fn format_prefixed_roundtrip() {
        let original = "1773653954.012345 O hello world";
        let parsed = LogLine::parse(original).expect("should parse");
        assert_eq!(parsed.format_prefixed(), original);
    }

    #[test]
    fn format_raw_strips_prefix() {
        let line = "1773653954.012345 O hello world";
        let parsed = LogLine::parse(line).expect("should parse");
        assert_eq!(parsed.format_raw(), "hello world");
    }

    // --- Test helpers ---

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

    // --- Task 2: query_log tests ---

    #[test]
    fn query_full_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        let query = LogQuery::default();
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 5);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("line one"));
        assert!(output.contains("line five with error word"));
    }

    #[test]
    fn query_tail_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        let query = LogQuery {
            tail: Some(2),
            ..Default::default()
        };
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 2);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("line four"));
        assert!(output.contains("line five with error word"));
        assert!(!output.contains("line one"));
    }

    #[test]
    fn query_tail_0_returns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        let query = LogQuery {
            tail: Some(0),
            ..Default::default()
        };
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn query_grep() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        let query = LogQuery {
            grep: Some("error".to_owned()),
            ..Default::default()
        };
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 2);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("error here"));
        assert!(output.contains("line five with error word"));
    }

    #[test]
    fn query_since() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        // since_us = 1000002_000000 should include lines at ts 1000002, 1000003, 1000004
        let query = LogQuery {
            since_us: Some(1_000_002_000_000),
            ..Default::default()
        };
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 3);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("error here"));
        assert!(output.contains("line four"));
        assert!(output.contains("line five"));
        assert!(!output.contains("line one"));
    }

    #[test]
    fn query_raw() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        let query = LogQuery {
            raw: true,
            ..Default::default()
        };
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 5);
        let output = String::from_utf8(buf).unwrap();
        // Raw output should not contain timestamp digits.
        assert!(!output.contains("1000000.000000"));
        assert!(output.contains("line one"));
    }

    #[test]
    fn query_combined_grep_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        // grep "error" gives 2 hits, tail 1 should give only the last one.
        let query = LogQuery {
            grep: Some("error".to_owned()),
            tail: Some(1),
            ..Default::default()
        };
        let count = query_log(&path, &query, &mut buf).unwrap();
        assert_eq!(count, 1);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("line five with error word"));
        assert!(!output.contains("error here"));
    }

    #[test]
    fn query_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.log");
        let mut buf = Vec::new();
        let query = LogQuery::default();
        let result = query_log(&path, &query, &mut buf);
        assert!(result.is_err());
    }

    // --- Task 3: follow_log tests ---

    #[test]
    fn follow_stops_on_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
        // With tail set, should read existing content then stop immediately.
        let query = LogQuery {
            tail: Some(100),
            ..Default::default()
        };
        follow_log(&path, &query, &mut buf, || true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("line one"));
        assert!(output.contains("line five"));
    }

    #[test]
    fn follow_picks_up_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.log");

        // Create initial file.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "1000000.000000 O initial line").unwrap();
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let path_clone = path.clone();

        let handle = thread::spawn(move || {
            let mut buf = Vec::new();
            let query = LogQuery {
                tail: Some(100),
                ..Default::default()
            };
            follow_log(&path_clone, &query, &mut buf, || {
                stop_clone.load(Ordering::Relaxed)
            })
            .unwrap();
            String::from_utf8(buf).unwrap()
        });

        // Give the follow thread time to start tailing.
        thread::sleep(Duration::from_millis(250));

        // Append a new line.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "1000001.000000 O appended line").unwrap();
        }

        // Give it time to pick up the new line, then stop.
        thread::sleep(Duration::from_millis(350));
        stop.store(true, Ordering::Relaxed);

        let output = handle.join().unwrap();
        assert!(output.contains("initial line"));
        assert!(output.contains("appended line"));
    }

    #[test]
    fn follow_waits_for_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delayed.log");

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let path_clone = path.clone();

        let handle = thread::spawn(move || {
            let mut buf = Vec::new();
            let query = LogQuery {
                tail: Some(100),
                ..Default::default()
            };
            follow_log(&path_clone, &query, &mut buf, || {
                stop_clone.load(Ordering::Relaxed)
            })
            .unwrap();
            String::from_utf8(buf).unwrap()
        });

        // Wait, then create the file.
        thread::sleep(Duration::from_millis(300));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "1000000.000000 O delayed line").unwrap();
        }

        // Let it pick up the line.
        thread::sleep(Duration::from_millis(350));
        stop.store(true, Ordering::Relaxed);

        let output = handle.join().unwrap();
        assert!(output.contains("delayed line"));
    }

    // --- Task 4: parse_since tests ---

    #[test]
    fn parse_since_epoch_seconds() {
        let result = parse_since("1000000").unwrap();
        assert_eq!(result, 1_000_000_000_000);
    }

    #[test]
    fn parse_since_seconds_duration() {
        let before = now_us();
        let result = parse_since("30s").unwrap();
        let after = now_us();
        // Result should be approximately now - 30s.
        let expected_low = before - 30_000_000;
        let expected_high = after - 30_000_000;
        assert!(
            result >= expected_low.saturating_sub(1000) && result <= expected_high + 1000,
            "result {result} not in expected range [{expected_low}, {expected_high}]"
        );
    }

    #[test]
    fn parse_since_minutes_duration() {
        let before = now_us();
        let result = parse_since("5m").unwrap();
        let after = now_us();
        let expected_low = before - 300_000_000;
        let expected_high = after - 300_000_000;
        assert!(
            result >= expected_low.saturating_sub(1000) && result <= expected_high + 1000,
            "result {result} not in expected range [{expected_low}, {expected_high}]"
        );
    }

    #[test]
    fn parse_since_invalid() {
        assert!(parse_since("abc").is_err());
        assert!(parse_since("5x").is_err());
        assert!(parse_since("").is_err());
    }
}
