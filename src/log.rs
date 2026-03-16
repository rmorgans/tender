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
}
