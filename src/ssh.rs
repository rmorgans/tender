use std::io;
use std::process::{Command, Stdio};

/// Errors from the SSH transport layer.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// Failed to spawn the ssh process itself.
    #[error("failed to spawn ssh: {0}")]
    SpawnFailed(io::Error),

    /// SSH process exited with a connection-level failure (exit code 255).
    #[error("ssh transport failed: connection or authentication failure (exit 255)")]
    TransportFailed,

    /// SSH process was killed by a signal or returned an unexpected OS error.
    #[error("ssh process terminated abnormally")]
    Abnormal,
}

/// Commands that are supported over SSH transport in the first slice.
const REMOTE_COMMANDS: &[&str] = &[
    "start", "status", "list", "log", "push", "kill", "wait", "watch", "attach",
];

/// Check whether a subcommand is supported for remote execution.
pub fn is_remote_supported(subcommand: &str) -> bool {
    REMOTE_COMMANDS.contains(&subcommand)
}

/// Build the SSH command line for remote tender invocation.
///
/// SSH sends everything after the destination as a single string to the
/// remote login shell. To preserve argument boundaries (spaces, quotes,
/// shell metacharacters), each tender argument is individually
/// shell-quoted using `shell_words::quote()` (POSIX quoting).
///
/// The resulting local argv is:
///   ssh -T -o ConnectTimeout=10 <host> tender 'arg1' 'arg2' ...
///
/// SSH concatenates "tender", "'arg1'", "'arg2'" with spaces and sends
/// the result to the remote login shell, which re-splits it into argv.
///
/// No `--` separator is used between the host and the remote command.
/// SSH does not treat `--` as an option terminator — it would be sent
/// literally to the remote shell as part of the command string.
///
/// **Scope:** POSIX remote login shells only (bash, zsh, sh, etc.).
/// Windows remote hosts (PowerShell, cmd.exe) need a different quoting
/// strategy and are deferred to a follow-on slice.
pub fn build_ssh_command(host: &str, tender_args: &[String], allocate_tty: bool) -> Command {
    let mut cmd = Command::new("ssh");
    let tty_flag = if allocate_tty { "-t" } else { "-T" };
    cmd.args([tty_flag, "-o", "ConnectTimeout=10", host]);

    // Each tender arg becomes a separate ssh argv entry, individually
    // quoted for the remote shell. SSH concatenates all args after the
    // host with spaces to form the remote command string.
    cmd.arg("tender");
    for arg in tender_args {
        cmd.arg(shell_words::quote(arg).into_owned());
    }

    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd
}

/// Execute a remote tender command via SSH, streaming stdout/stderr
/// directly to the local stdout/stderr.
///
/// Returns the remote tender exit code on success.
/// Returns `SshError::TransportFailed` for SSH connection failures (exit 255).
pub fn exec_ssh(host: &str, tender_args: &[String], allocate_tty: bool) -> Result<i32, SshError> {
    let mut child = build_ssh_command(host, tender_args, allocate_tty)
        .spawn()
        .map_err(SshError::SpawnFailed)?;

    let status = child.wait().map_err(SshError::SpawnFailed)?;

    match status.code() {
        Some(255) => Err(SshError::TransportFailed),
        Some(code) => Ok(code),
        None => Err(SshError::Abnormal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: extract the args that `build_ssh_command` would pass to the ssh binary.
    fn ssh_argv(host: &str, tender_args: &[&str]) -> Vec<String> {
        let args: Vec<String> = tender_args.iter().map(|s| s.to_string()).collect();
        let cmd = build_ssh_command(host, &args, false);
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn simple_command() {
        let args = ssh_argv("user@box", &["status", "my-session"]);
        assert_eq!(args[0], "-T");
        assert_eq!(args[1], "-o");
        assert_eq!(args[2], "ConnectTimeout=10");
        assert_eq!(args[3], "user@box");
        assert_eq!(args[4], "tender");
        assert_eq!(args[5], "status");
        assert_eq!(args[6], "my-session");
        assert!(!args.contains(&"--".to_string()), "no -- in ssh argv: {args:?}");
    }

    #[test]
    fn args_with_spaces_are_quoted() {
        let args: Vec<String> = vec![
            "start".to_string(),
            "job".to_string(),
            "--".to_string(),
            "echo".to_string(),
            "hello world".to_string(),
        ];
        let cmd = build_ssh_command("user@box", &args, false);
        let argv: Vec<String> = cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let remote_args = &argv[4..];
        let remote_cmd = remote_args.join(" ");
        let parsed = shell_words::split(&remote_cmd).unwrap();
        assert_eq!(parsed, vec!["tender", "start", "job", "--", "echo", "hello world"]);
    }

    #[test]
    fn args_with_shell_metacharacters_are_quoted() {
        let args: Vec<String> = vec![
            "start".to_string(),
            "job".to_string(),
            "--".to_string(),
            "bash".to_string(),
            "-c".to_string(),
            "echo $HOME && ls".to_string(),
        ];
        let cmd = build_ssh_command("user@box", &args, false);
        let argv: Vec<String> = cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let remote_args = &argv[4..];
        let remote_cmd = remote_args.join(" ");
        let parsed = shell_words::split(&remote_cmd).unwrap();
        assert_eq!(parsed[6], "echo $HOME && ls");
    }

    #[test]
    fn remote_allowlist() {
        assert!(is_remote_supported("start"));
        assert!(is_remote_supported("status"));
        assert!(is_remote_supported("list"));
        assert!(is_remote_supported("log"));
        assert!(is_remote_supported("push"));
        assert!(is_remote_supported("kill"));
        assert!(is_remote_supported("wait"));
        assert!(is_remote_supported("watch"));
        assert!(is_remote_supported("attach"));

        assert!(!is_remote_supported("run"));
        assert!(!is_remote_supported("exec"));
        assert!(!is_remote_supported("wrap"));
        assert!(!is_remote_supported("prune"));
        assert!(!is_remote_supported("_sidecar"));
    }
}
