/// Build a framed shell command string for Unix shells (bash/sh).
///
/// The command is escaped using shell_words::join, then appended with a sentinel
/// trailer that captures exit code and cwd.
///
/// Token must be hex-only (as produced by `generate_token`).
pub fn unix_frame(argv: &[String], token: &str) -> String {
    debug_assert!(
        token.bytes().all(|b| b.is_ascii_hexdigit()),
        "token must be hex-only, got: {token}"
    );
    let cmd = shell_words::join(argv);
    format!(
        "{cmd}; __tender_s=$?; printf '__TENDER_EXEC__ %s %s %s\\n' '{token}' \"$__tender_s\" \"$(pwd)\"\n"
    )
}

/// Build a framed command string for PowerShell.
///
/// Note: argument escaping is minimal (space-joined). Sufficient for first slice.
/// Token must be hex-only (as produced by `generate_token`).
pub fn powershell_frame(argv: &[String], token: &str) -> String {
    debug_assert!(
        token.bytes().all(|b| b.is_ascii_hexdigit()),
        "token must be hex-only, got: {token}"
    );
    let cmd = argv.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(" ");
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
    format!("{:x}{:x}", std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_frame_simple_command() {
        let frame = unix_frame(&["echo".into(), "hello".into()], "a1b2c3");
        assert!(frame.contains("echo hello"));
        assert!(frame.contains("__TENDER_EXEC__ %s %s %s"));
        assert!(frame.contains("a1b2c3"));
        assert!(frame.ends_with('\n'));
    }

    #[test]
    fn unix_frame_command_with_special_chars() {
        let frame = unix_frame(&["echo".into(), "it's a \"test\"".into()], "a1b2c3");
        assert!(frame.contains("__TENDER_EXEC__"));
        assert!(frame.contains("a1b2c3"));
    }

    #[test]
    fn parse_sentinel_valid() {
        let result = parse_sentinel("__TENDER_EXEC__ a1b2c3 0 /home/user", "a1b2c3");
        assert!(result.is_some());
        let (exit_code, cwd) = result.unwrap();
        assert_eq!(exit_code, 0);
        assert_eq!(cwd, "/home/user");
    }

    #[test]
    fn parse_sentinel_nonzero_exit() {
        let result = parse_sentinel("__TENDER_EXEC__ a1b2c3 42 /tmp", "a1b2c3");
        let (exit_code, cwd) = result.unwrap();
        assert_eq!(exit_code, 42);
        assert_eq!(cwd, "/tmp");
    }

    #[test]
    fn parse_sentinel_cwd_with_spaces() {
        let result = parse_sentinel("__TENDER_EXEC__ a1b2c3 0 /home/user/my project", "a1b2c3");
        let (_, cwd) = result.unwrap();
        assert_eq!(cwd, "/home/user/my project");
    }

    #[test]
    fn parse_sentinel_wrong_token() {
        let result = parse_sentinel("__TENDER_EXEC__ deadbeef 0 /home", "a1b2c3");
        assert!(result.is_none());
    }

    #[test]
    fn parse_sentinel_not_sentinel() {
        let result = parse_sentinel("hello world", "a1b2c3");
        assert!(result.is_none());
    }

    #[test]
    fn powershell_frame_simple_command() {
        let frame = powershell_frame(&["echo".into(), "hello".into()], "abc123");
        assert!(frame.contains("echo hello"));
        assert!(frame.contains("__TENDER_EXEC__ abc123"));
        assert!(frame.contains("$LASTEXITCODE"));
        assert!(frame.ends_with('\n'));
    }

    #[test]
    fn generate_token_is_unique() {
        let t1 = generate_token();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }
}
