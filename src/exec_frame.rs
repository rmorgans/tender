use base64::Engine;

/// Build a framed Python exec string for injection into a Python REPL.
///
/// Both the user's code AND the result path are base64-encoded to avoid any
/// escaping issues with backslashes, quotes, or special characters in paths
/// (especially Windows paths containing `\t`, `\U`, etc.).
///
/// The frame wraps execution in try/except, captures stdout/stderr/cwd/traceback,
/// and writes a JSON result file atomically (tmp + rename).
///
/// `result_path` is the absolute path to `{session_dir}/exec-results/{token}.json`.
pub fn python_frame(code: &str, result_path: &str) -> String {
    let encoded_code = base64::engine::general_purpose::STANDARD.encode(code);
    let encoded_path = base64::engine::general_purpose::STANDARD.encode(result_path);
    // The entire frame must be a single line for REPL injection.
    // Python's compile() interprets \n as real newlines inside the string.
    format!(
        "exec(compile('import json,os,sys,contextlib,io,traceback,base64 as _b64;_out,_err,_code,_tb=io.StringIO(),io.StringIO(),0,None;_rp=_b64.b64decode(\"{encoded_path}\").decode()\\ntry:\\n with contextlib.redirect_stdout(_out),contextlib.redirect_stderr(_err):\\n  exec(compile(_b64.b64decode(\"{encoded_code}\").decode(),\"<exec>\",\"exec\"))\\nexcept SystemExit as _e:\\n _code=_e.code if _e.code is not None else 0\\nexcept:\\n _tb=traceback.format_exc();_code=1\\n_tmp=_rp+\".tmp\"\\nwith open(_tmp,\"w\") as _f:\\n json.dump(dict(exit_code=_code,cwd=os.getcwd(),stdout=_out.getvalue(),stderr=_err.getvalue(),traceback=_tb),_f)\\nos.rename(_tmp,_rp)','<tender-exec>','exec'))\n"
    )
}

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
/// Each argv element is passed as a single-quoted PowerShell string literal and
/// invoked through the call operator so spaces and metacharacters survive
/// round-tripping through the shell.
///
/// Token must be hex-only (as produced by `generate_token`).
pub fn powershell_frame(argv: &[String], token: &str) -> String {
    debug_assert!(
        token.bytes().all(|b| b.is_ascii_hexdigit()),
        "token must be hex-only, got: {token}"
    );
    let cmd = argv
        .iter()
        .map(|arg| powershell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "$LASTEXITCODE = $null; & {cmd}; $__tender_s = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}; Write-Output ('__TENDER_EXEC__ {token} ' + $__tender_s + ' ' + (Get-Location).Path)\n"
    )
}

fn powershell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "''"))
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
        assert!(frame.starts_with("$LASTEXITCODE = $null; & 'echo' 'hello'"));
        assert!(frame.contains("__TENDER_EXEC__ abc123"));
        assert!(frame.contains("$LASTEXITCODE"));
        assert!(frame.contains("(Get-Location).Path"));
        assert!(frame.ends_with('\n'));
    }

    #[test]
    fn powershell_frame_quotes_special_chars() {
        let frame = powershell_frame(
            &[
                "Write-Output".into(),
                "a b".into(),
                "$HOME".into(),
                "it's `quoted`;".into(),
            ],
            "abc123",
        );
        assert!(frame.contains("& 'Write-Output' 'a b' '$HOME' 'it''s `quoted`;'"));
        assert!(frame.ends_with('\n'));
    }

    #[test]
    fn python_frame_encodes_code_and_path() {
        let frame = python_frame("print('hello')", "/tmp/result.json");
        assert!(frame.contains("b64decode"));
        assert!(frame.ends_with('\n'));
        // Both code and path are base64-encoded — neither appears raw
        assert!(!frame.contains("/tmp/result.json"));
        let encoded_code = base64::engine::general_purpose::STANDARD.encode("print('hello')");
        let encoded_path = base64::engine::general_purpose::STANDARD.encode("/tmp/result.json");
        assert!(frame.contains(&encoded_code));
        assert!(frame.contains(&encoded_path));
    }

    #[test]
    fn python_frame_handles_special_chars() {
        // Code with quotes, newlines, backslashes — all handled by base64
        let code = "x = 'hello'\nprint(f\"value: {x}\\n\")";
        let frame = python_frame(code, "/tmp/result.json");
        assert!(frame.contains("b64decode"));
        // Frame is a single line
        assert_eq!(frame.matches('\n').count(), 1); // trailing newline only
    }

    #[test]
    fn python_frame_escapes_windows_paths() {
        // Windows path with backslashes that would be problematic if not b64-encoded
        let frame = python_frame("print(1)", r"C:\Users\test\exec-results\abc.json");
        assert!(!frame.contains(r"C:\Users")); // path must not appear raw
        assert!(frame.contains("b64decode"));
    }

    #[test]
    fn generate_token_is_unique() {
        let t1 = generate_token();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }
}
