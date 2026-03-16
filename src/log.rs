use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

/// Query parameters for filtering log output.
pub struct LogQuery {
    pub tail: Option<usize>,
    pub grep: Option<String>,
    pub since_us: Option<u64>,
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

/// Read and filter a log file, writing matching lines to `out`.
///
/// Returns the number of lines written. Malformed lines are silently skipped.
pub fn query_log(path: &Path, query: &LogQuery, out: &mut dyn Write) -> io::Result<usize> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut lines: Vec<LogLine> = Vec::new();
    for raw_line in reader.lines() {
        let raw_line = raw_line?;
        let Some(parsed) = LogLine::parse(&raw_line) else {
            continue;
        };

        if let Some(threshold) = query.since_us {
            if parsed.timestamp_us < threshold {
                continue;
            }
        }

        if let Some(ref pattern) = query.grep {
            if !parsed.content.contains(pattern.as_str()) {
                continue;
            }
        }

        lines.push(parsed);
    }

    if let Some(n) = query.tail {
        let start = lines.len().saturating_sub(n);
        lines = lines.split_off(start);
    }

    let count = lines.len();
    for line in &lines {
        if query.raw {
            writeln!(out, "{}", line.format_raw())?;
        } else {
            writeln!(out, "{}", line.format_prefixed())?;
        }
    }

    Ok(count)
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
            timestamp_us: secs
                .checked_mul(1_000_000)?
                .checked_add(micros)?,
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

    // --- query_log tests ---

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
        assert!(!output.contains("1000000.000000"));
        assert!(output.contains("line one"));
    }

    #[test]
    fn query_combined_grep_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_test_log(dir.path());
        let mut buf = Vec::new();
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
}
