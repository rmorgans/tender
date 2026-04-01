# Remote SSH Transport Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `--host` flag to route supported tender commands through SSH to a remote tender instance, preserving all JSON/NDJSON contracts.

**Architecture:** A thin `ssh` module wraps the system `ssh` client. When `--host` is present, the CLI checks an allowlist of remote-supported commands, then builds an SSH invocation. Each forwarded argument is individually shell-quoted for the remote side (SSH concatenates all args after the destination into a single string passed to the remote login shell). stdout/stderr are streamed through directly. Exit codes are preserved when `tender` is reachable; SSH transport failures get a distinct error path.

**Key design decisions:**
- **Allowlist, not passthrough:** Only `start`, `status`, `list`, `log`, `push`, `kill`, `wait`, and `watch` are remote-supported. `run`, `exec`, `wrap`, `prune`, and `_sidecar` are rejected with a clear error.
- **POSIX remote hosts only (slice one):** The quoting strategy uses `shell_words::quote()`, which produces POSIX-shell-safe quoting. This means the remote host must have a POSIX-compatible login shell (bash, zsh, sh, etc.). Windows remote hosts with PowerShell or cmd.exe are explicitly out of scope for this slice. The backlog spec's note about Windows remote targets is deferred to a follow-on slice that would need a separate quoting strategy.
- **Remote shell quoting:** Arguments are individually `shell_words::quote()`-d and joined into a single remote command string. SSH receives this as one argument after the host: `ssh -T host 'tender arg1 arg2'`. No `--` separator is used between the SSH destination and the remote command — SSH does not use `--` as an option terminator in the same way as most CLI tools. Everything after the destination is the remote command.
- **`--host` stripping uses clap, not raw argv scanning:** The host value comes from clap's parsed `Cli.host` field. The forwarded args are reconstructed from clap's parsed `Commands` variant, not by scanning raw `std::env::args()`. This avoids the bug where a child command's `--host` argument (after `--`) would be silently eaten.

**Tech Stack:** Rust, clap 4 (global `--host` flag), `std::process::Command` for SSH invocation, `shell_words` (already a dependency) for POSIX remote quoting, existing serde_json for output validation in tests.

---

### Task 0: Promote spec from backlog to active

Collapse the two plan sources of truth before any implementation starts.
The canonical spec at `docs/plans/backlog/remote-ssh-transport.md` moves to
`docs/plans/active/` so there is one authoritative location.

**Step 1: Move the spec and add scope note**

```bash
git mv docs/plans/backlog/remote-ssh-transport.md docs/plans/active/remote-ssh-transport.md
```

Add a note at the top of the moved spec clarifying the first-slice scope:

> **Slice one scope:** POSIX remote hosts only. Windows remote hosts
> (PowerShell, cmd.exe) are deferred — the POSIX shell quoting strategy
> used in this slice does not apply to Windows remote shells.

**Step 2: Commit**

```bash
git add docs/plans/active/remote-ssh-transport.md
git commit -m "docs(plans): promote remote-ssh-transport to active, scope to POSIX remotes"
```

---

### Task 1: Add `--host` global flag to CLI parser

**Files:**
- Modify: `src/main.rs:9-14` (Cli struct)

**Step 1: Write the failing test**

Create test file `tests/cli_remote.rs`:

```rust
mod harness;

use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn host_flag_is_accepted_by_parser() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@example.com", "list"])
        .output()
        .unwrap();

    // Should not fail with "unexpected argument '--host'"
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "parser should accept --host, got: {stderr}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test cli_remote host_flag_is_accepted_by_parser`
Expected: FAIL — clap rejects `--host` as unknown argument.

**Step 3: Add `--host` field to `Cli` struct**

In `src/main.rs`, add the field to the `Cli` struct:

```rust
#[derive(Parser)]
#[command(name = "tender", about = "Agent process sitter")]
struct Cli {
    /// Route command through SSH to a remote host (e.g. user@box)
    #[arg(long, global = true)]
    host: Option<String>,

    #[command(subcommand)]
    command: Commands,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test cli_remote host_flag_is_accepted_by_parser`
Expected: PASS (the flag is parsed; the command will fail for other reasons like no remote host, but the parser accepts it).

**Step 5: Commit**

```bash
git add src/main.rs tests/cli_remote.rs
git commit -m "feat(remote): add --host global CLI flag"
```

---

### Task 2: Create SSH transport module

**Files:**
- Create: `src/ssh.rs`
- Modify: `src/lib.rs` (add `pub mod ssh;`)

**Step 1: Write the stub and add module to lib.rs**

Add `pub mod ssh;` to `src/lib.rs`.

Create `src/ssh.rs` with just enough to compile:

```rust
// src/ssh.rs — stub
```

**Step 2: Write the implementation**

Create `src/ssh.rs`:

```rust
use std::io;
use std::process::{Command, Stdio};

/// Errors from the SSH transport layer.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// Failed to spawn the ssh process itself.
    #[error("failed to spawn ssh: {0}")]
    SpawnFailed(io::Error),

    /// SSH process exited with a connection-level failure (exit code 255).
    #[error("ssh transport failed (exit 255): {detail}")]
    TransportFailed { detail: String },

    /// SSH process was killed by a signal or returned an unexpected OS error.
    #[error("ssh process terminated abnormally")]
    Abnormal,
}

/// Commands that are supported over SSH transport in the first slice.
const REMOTE_COMMANDS: &[&str] = &[
    "start", "status", "list", "log", "push", "kill", "wait", "watch",
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
pub fn build_ssh_command(host: &str, tender_args: &[String]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-T", "-o", "ConnectTimeout=10", host]);

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
pub fn exec_ssh(host: &str, tender_args: &[String]) -> Result<i32, SshError> {
    let mut child = build_ssh_command(host, tender_args)
        .spawn()
        .map_err(SshError::SpawnFailed)?;

    let status = child.wait().map_err(SshError::SpawnFailed)?;

    match status.code() {
        Some(255) => Err(SshError::TransportFailed {
            detail: "ssh exited with code 255 (connection or auth failure)".to_string(),
        }),
        Some(code) => Ok(code),
        None => Err(SshError::Abnormal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// Helper: extract the args that `build_ssh_command` would pass to the ssh binary.
    fn ssh_argv(host: &str, tender_args: &[&str]) -> Vec<String> {
        let args: Vec<String> = tender_args.iter().map(|s| s.to_string()).collect();
        let cmd = build_ssh_command(host, &args);
        // cmd.get_args() returns the args after the program name
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn simple_command() {
        let args = ssh_argv("user@box", &["status", "my-session"]);
        // ssh -T -o ConnectTimeout=10 user@box tender status my-session
        assert_eq!(args[0], "-T");
        assert_eq!(args[1], "-o");
        assert_eq!(args[2], "ConnectTimeout=10");
        assert_eq!(args[3], "user@box");
        assert_eq!(args[4], "tender");
        assert_eq!(args[5], "status");
        assert_eq!(args[6], "my-session");
        // No "--" between host and remote command
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
        let cmd = build_ssh_command("user@box", &args);
        let argv: Vec<String> = cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // SSH will concatenate all args after the host with spaces to
        // form the remote command string. Simulate that and verify
        // round-trip through POSIX shell splitting.
        let remote_args = &argv[4..]; // skip -T -o ConnectTimeout=10 host
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
        let cmd = build_ssh_command("user@box", &args);
        let argv: Vec<String> = cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let remote_args = &argv[4..];
        let remote_cmd = remote_args.join(" ");
        let parsed = shell_words::split(&remote_cmd).unwrap();
        assert_eq!(parsed[5], "echo $HOME && ls");
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

        assert!(!is_remote_supported("run"));
        assert!(!is_remote_supported("exec"));
        assert!(!is_remote_supported("wrap"));
        assert!(!is_remote_supported("prune"));
        assert!(!is_remote_supported("_sidecar"));
    }
}
```

**Step 3: Run tests to verify**

Run: `cargo test ssh::tests`
Expected: All 4 unit tests PASS.

**Step 4: Commit**

```bash
git add src/ssh.rs src/lib.rs
git commit -m "feat(remote): add SSH transport module with quoting and allowlist"
```

---

### Task 3: Add `remote_args()` method to `Commands` enum and wire dispatch

The key insight: **do not scan raw `std::env::args()`**. Instead, reconstruct
the forwarded args from clap's already-parsed `Commands` variant. This avoids
the bug where a child command's `--host` argument (after `--`) would be silently
eaten by a naive token filter.

**Files:**
- Modify: `src/main.rs` (add `remote_args()` method + dispatch in `main()`)

**Step 1: Write the failing test**

Add to `tests/cli_remote.rs`:

```rust
use tempfile::TempDir;

/// Helper: create a fake ssh script that dumps args to stdout.
/// Returns the TempDir (must be kept alive for PATH to stay valid).
#[cfg(unix)]
fn fake_ssh_echo() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    // $1 is the raw command string SSH would send to the remote shell.
    // We print all args so tests can inspect what was passed.
    std::fs::write(&fake_ssh, "#!/bin/sh\nfor arg in \"$@\"; do echo \"ARG:$arg\"; done\n").unwrap();
    std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();
    tmp
}

#[test]
fn host_flag_invokes_ssh_with_correct_remote_command() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let args: Vec<&str> = stdout.lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .collect();
    // Should see: -T, -o, ConnectTimeout=10, user@box, tender, list
    assert!(args.contains(&"tender"), "should contain tender: {args:?}");
    assert!(args.contains(&"list"), "should contain list: {args:?}");
    // No "--" should be passed to ssh
    assert!(!args.contains(&"--"), "should not contain -- in ssh args: {args:?}");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test cli_remote host_flag_invokes_ssh`
Expected: FAIL — `--host` is parsed but not acted on; local `list` runs instead.

**Step 3: Add `remote_args()` to `Commands` and dispatch in `main()`**

Add a method to `Commands` that reconstructs the subcommand's args from the
parsed clap fields. This is the only place that knows the mapping from parsed
fields back to CLI tokens. It returns `Vec<String>` — the exact args that the
remote `tender` should receive.

```rust
impl Commands {
    /// Reconstruct CLI args for this command, suitable for remote invocation.
    /// Returns (subcommand_name, args) where args does NOT include the subcommand itself.
    fn remote_args(&self) -> (&'static str, Vec<String>) {
        match self {
            Commands::Start {
                name, namespace, stdin, replace, timeout, cwd, env_vars,
                on_exit, after, any_exit, cmd,
            } => {
                let mut args = vec!["start".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if *stdin { args.push("--stdin".to_string()); }
                if *replace { args.push("--replace".to_string()); }
                if let Some(t) = timeout { args.extend(["--timeout".to_string(), t.to_string()]); }
                if let Some(c) = cwd { args.extend(["--cwd".to_string(), c.display().to_string()]); }
                for e in env_vars { args.extend(["--env".to_string(), e.clone()]); }
                for o in on_exit { args.extend(["--on-exit".to_string(), o.clone()]); }
                for a in after { args.extend(["--after".to_string(), a.clone()]); }
                if *any_exit { args.push("--any-exit".to_string()); }
                args.push("--".to_string());
                args.extend(cmd.iter().cloned());
                ("start", args)
            }
            Commands::Status { name, namespace } => {
                let mut args = vec!["status".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                ("status", args)
            }
            Commands::List { namespace } => {
                let mut args = vec!["list".to_string()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                ("list", args)
            }
            Commands::Log { name, namespace, tail, follow, grep, since, raw } => {
                let mut args = vec!["log".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if let Some(n) = tail { args.extend(["--tail".to_string(), n.to_string()]); }
                if *follow { args.push("--follow".to_string()); }
                if let Some(g) = grep { args.extend(["--grep".to_string(), g.clone()]); }
                if let Some(s) = since { args.extend(["--since".to_string(), s.clone()]); }
                if *raw { args.push("--raw".to_string()); }
                ("log", args)
            }
            Commands::Push { name, namespace } => {
                let mut args = vec!["push".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                ("push", args)
            }
            Commands::Kill { name, namespace, force } => {
                let mut args = vec!["kill".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if *force { args.push("--force".to_string()); }
                ("kill", args)
            }
            Commands::Wait { name, namespace, timeout } => {
                let mut args = vec!["wait".to_string(), name.clone()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if let Some(t) = timeout { args.extend(["--timeout".to_string(), t.to_string()]); }
                ("wait", args)
            }
            Commands::Watch { namespace, events, logs, annotations, from_now } => {
                let mut args = vec!["watch".to_string()];
                if let Some(ns) = namespace { args.extend(["--namespace".to_string(), ns.clone()]); }
                if *events { args.push("--events".to_string()); }
                if *logs { args.push("--logs".to_string()); }
                if *annotations { args.push("--annotations".to_string()); }
                if *from_now { args.push("--from-now".to_string()); }
                ("watch", args)
            }
            // Unsupported commands — these should never reach here because
            // main() checks is_remote_supported() first, but be explicit.
            _ => unreachable!("remote_args called on unsupported command"),
        }
    }

    /// Return the subcommand name string for allowlist checking.
    fn name(&self) -> &'static str {
        match self {
            Commands::Start { .. } => "start",
            Commands::Status { .. } => "status",
            Commands::List { .. } => "list",
            Commands::Log { .. } => "log",
            Commands::Push { .. } => "push",
            Commands::Kill { .. } => "kill",
            Commands::Wait { .. } => "wait",
            Commands::Watch { .. } => "watch",
            Commands::Run { .. } => "run",
            Commands::Exec { .. } => "exec",
            Commands::Wrap { .. } => "wrap",
            Commands::Prune { .. } => "prune",
            Commands::Sidecar { .. } => "_sidecar",
        }
    }
}
```

Then in `main()`, add the dispatch before the existing match:

```rust
fn main() {
    let cli = Cli::parse();

    // If --host is set, dispatch to SSH transport.
    if let Some(ref host) = cli.host {
        let cmd_name = cli.command.name();
        if !tender::ssh::is_remote_supported(cmd_name) {
            eprintln!("command '{cmd_name}' is not supported over SSH (--host)");
            std::process::exit(1);
        }
        let (_name, args) = cli.command.remote_args();
        match tender::ssh::exec_ssh(host, &args) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    let result = match cli.command {
        // ... existing match arms unchanged ...
    };

    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test cli_remote host_flag_invokes_ssh`
Expected: PASS.

**Step 5: Run full test suite**

Run: `cargo test`
Expected: All existing tests still pass (no `--host` means local path unchanged).

**Step 6: Commit**

```bash
git add src/main.rs tests/cli_remote.rs
git commit -m "feat(remote): wire --host to SSH dispatch with clap-based arg reconstruction"
```

---

### Task 4: Test SSH transport error classification

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn host_flag_exit_255_is_transport_error() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    #[cfg(unix)]
    {
        std::fs::write(&fake_ssh, "#!/bin/sh\nexit 255\n").unwrap();
        std::fs::set_permissions(&fake_ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "transport error should exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("transport"),
        "stderr should mention transport failure, got: {stderr}"
    );
}

#[test]
fn host_flag_preserves_remote_exit_code() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    #[cfg(unix)]
    {
        // Simulate remote tender exiting with code 42 (ExitedError)
        std::fs::write(&fake_ssh, "#!/bin/sh\nexit 42\n").unwrap();
        std::fs::set_permissions(&fake_ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "wait", "my-session"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(42),
        "should preserve remote exit code 42"
    );
}
```

**Step 2: Run tests to verify they pass**

Run: `cargo test --test cli_remote`
Expected: PASS — the SSH dispatch already handles exit 255 as transport error and passes through other codes.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify transport error classification and exit code passthrough"
```

---

### Task 5: Test remote JSON output passthrough

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the test**

```rust
#[test]
fn host_flag_passes_through_json_stdout() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    #[cfg(unix)]
    {
        // Simulate remote tender returning JSON status output
        let script = r#"#!/bin/sh
cat <<'ENDJSON'
{
  "schema_version": 1,
  "session": "remote-job",
  "status": "Running"
}
ENDJSON
exit 0
"#;
        std::fs::write(&fake_ssh, script).unwrap();
        std::fs::set_permissions(&fake_ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "status", "remote-job"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect(&format!("stdout should be valid JSON, got: {stdout}"));
    assert_eq!(parsed["session"], "remote-job");
    assert_eq!(parsed["status"], "Running");
    assert!(output.status.success());
}
```

**Step 2: Run test**

Run: `cargo test --test cli_remote host_flag_passes_through_json`
Expected: PASS — stdout is inherited, so JSON passes through unmodified.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify JSON output passthrough over SSH"
```

---

### Task 6: Test remote NDJSON streaming passthrough

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the test**

```rust
#[test]
fn host_flag_passes_through_ndjson_stream() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    #[cfg(unix)]
    {
        // Simulate remote tender watch producing NDJSON
        let script = r#"#!/bin/sh
echo '{"ts":1.0,"namespace":"default","session":"s1","run_id":"abc","source":"tender.sidecar","kind":"run","name":"run.started","data":{"status":"Running"}}'
echo '{"ts":2.0,"namespace":"default","session":"s1","run_id":"abc","source":"tender.sidecar","kind":"run","name":"run.exited","data":{"status":"Exited","reason":"ExitedOk","exit_code":0}}'
exit 0
"#;
        std::fs::write(&fake_ssh, script).unwrap();
        std::fs::set_permissions(&fake_ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "watch", "--events"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "should have 2 NDJSON lines, got: {stdout}");

    for line in &lines {
        let event: serde_json::Value =
            serde_json::from_str(line).expect(&format!("each line should be valid JSON: {line}"));
        assert_eq!(event["source"], "tender.sidecar");
    }

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["name"], "run.started");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["name"], "run.exited");
}
```

**Step 2: Run test**

Run: `cargo test --test cli_remote host_flag_passes_through_ndjson`
Expected: PASS.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify NDJSON streaming passthrough over SSH"
```

---

### Task 7: Test SSH spawn failure (ssh not found)

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the test**

```rust
#[test]
fn host_flag_ssh_not_found_gives_clear_error() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    // Empty PATH — ssh binary won't be found
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "list"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "should fail when ssh not found");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ssh") || stderr.contains("spawn"),
        "error should mention ssh, got: {stderr}"
    );
}
```

**Step 2: Run test**

Run: `cargo test --test cli_remote host_flag_ssh_not_found`
Expected: PASS.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify clear error when ssh binary not found"
```

---

### Task 8: Test that `--host` works with `--namespace` and `--host` is not forwarded

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the test**

```rust
#[test]
fn host_flag_forwards_namespace_and_strips_host() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "status", "my-session", "--namespace", "prod"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Find the remote command string (last ARG: line)
    let remote_cmd = stdout.lines()
        .filter(|l| l.starts_with("ARG:"))
        .last()
        .expect("should have ARG lines");

    assert!(
        remote_cmd.contains("--namespace") && remote_cmd.contains("prod"),
        "namespace flag should be forwarded to remote, got: {remote_cmd}"
    );
    assert!(
        !remote_cmd.contains("--host"),
        "--host should NOT appear in remote command, got: {remote_cmd}"
    );
}
```

**Step 2: Run test**

Run: `cargo test --test cli_remote host_flag_forwards_namespace`
Expected: PASS — args are reconstructed from clap, `--host` is never included.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify --namespace forwarded, --host not forwarded"
```

---

### Task 9: Test remote stderr passthrough

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the test**

```rust
#[test]
fn host_flag_passes_through_stderr() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::TempDir::new().unwrap();
    let fake_ssh = tmp.path().join("ssh");
    #[cfg(unix)]
    {
        let script = "#!/bin/sh\necho 'session not found: oops' >&2\nexit 1\n";
        std::fs::write(&fake_ssh, script).unwrap();
        std::fs::set_permissions(&fake_ssh, PermissionsExt::from_mode(0o755)).unwrap();
    }

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "status", "oops"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found"),
        "remote stderr should pass through, got: {stderr}"
    );
    assert_eq!(output.status.code(), Some(1));
}
```

**Step 2: Run test**

Run: `cargo test --test cli_remote host_flag_passes_through_stderr`
Expected: PASS.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify stderr passthrough from remote"
```

---

### Task 10: Test `--host` with `start` command — trailing args and quoting

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the tests**

Two tests: basic trailing args, and args with spaces/metacharacters that exercise the quoting path.

```rust
/// Helper: extract the remote command from the fake ssh output.
///
/// `fake_ssh_echo()` prints each arg as `ARG:<value>`. The remote
/// command args are everything after the host (arg index 4+, since
/// the first 4 are: -T, -o, ConnectTimeout=10, <host>).
///
/// SSH concatenates these with spaces to form the remote command string.
/// We do the same and then `shell_words::split()` to simulate what the
/// remote POSIX shell would produce as argv.
fn parse_remote_argv(stdout: &str) -> Vec<String> {
    let args: Vec<&str> = stdout.lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .collect();
    // Skip: -T, -o, ConnectTimeout=10, <host>
    let remote_parts = &args[4..];
    let remote_cmd = remote_parts.join(" ");
    shell_words::split(&remote_cmd)
        .expect("remote command should be valid POSIX shell syntax")
}

#[test]
fn host_flag_forwards_start_with_trailing_args() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args(["--host", "user@box", "start", "job", "--timeout", "30", "--", "sleep", "60"])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);

    assert_eq!(parsed[0], "tender");
    assert!(parsed.contains(&"start".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"job".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"--timeout".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"30".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"sleep".to_string()), "parsed: {parsed:?}");
    assert!(parsed.contains(&"60".to_string()), "parsed: {parsed:?}");
    // --host must NOT be in the remote command
    assert!(!parsed.contains(&"--host".to_string()), "parsed: {parsed:?}");
}

#[test]
fn host_flag_quotes_child_args_with_spaces() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    // The child command has args with spaces and shell metacharacters.
    // This is the critical quoting test.
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args([
            "--host", "user@box",
            "start", "job", "--",
            "echo", "hello world", "foo;bar", "$HOME",
        ])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);

    // tender start job -- echo "hello world" "foo;bar" "$HOME"
    assert_eq!(parsed[0], "tender");
    assert!(parsed.contains(&"hello world".to_string()),
        "space-containing arg must survive round-trip: {parsed:?}");
    assert!(parsed.contains(&"foo;bar".to_string()),
        "semicolon-containing arg must survive: {parsed:?}");
    assert!(parsed.contains(&"$HOME".to_string()),
        "dollar sign must survive (not expanded): {parsed:?}");
}

#[test]
fn host_flag_does_not_eat_child_host_arg() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = fake_ssh_echo();

    // The child command itself has --host as an argument.
    // This must NOT be stripped — only the top-level --host is consumed by clap.
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
        .args([
            "--host", "user@box",
            "start", "job", "--",
            "myprog", "--host", "other-host",
        ])
        .env("PATH", tmp.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_remote_argv(&stdout);

    // The child's --host and its value must be present
    let host_indices: Vec<usize> = parsed.iter().enumerate()
        .filter(|(_, a)| a.as_str() == "--host")
        .map(|(i, _)| i)
        .collect();
    assert!(!host_indices.is_empty(),
        "child's --host must be preserved in remote command: {parsed:?}");
    // And "other-host" must follow it
    for &i in &host_indices {
        if i + 1 < parsed.len() && parsed[i + 1] == "other-host" {
            return; // found it
        }
    }
    panic!("child's --host other-host must be preserved: {parsed:?}");
}
```

**Step 2: Run tests**

Run: `cargo test --test cli_remote host_flag_forwards_start host_flag_quotes_child host_flag_does_not_eat`
Expected: All PASS.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify quoting, trailing args, and child --host preservation"
```

---

### Task 11: Test allowlist rejects unsupported commands over SSH

**Files:**
- Modify: `tests/cli_remote.rs`

**Step 1: Write the test**

```rust
#[test]
fn host_flag_rejects_unsupported_commands() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    // These commands should be rejected with a clear error, never reaching SSH.
    for cmd_args in &[
        vec!["--host", "user@box", "run", "script.sh"],
        vec!["--host", "user@box", "exec", "session", "--", "ls"],
        vec!["--host", "user@box", "wrap", "--source", "test.hook", "--event", "test", "--", "true"],
        vec!["--host", "user@box", "prune", "--all"],
    ] {
        let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("tender"))
            .args(cmd_args)
            .output()
            .unwrap();

        assert!(
            !output.status.success(),
            "command {:?} should be rejected over SSH",
            cmd_args
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not supported"),
            "should mention 'not supported' for {:?}, got: {stderr}",
            cmd_args
        );
    }
}
```

**Step 2: Run test**

Run: `cargo test --test cli_remote host_flag_rejects_unsupported`
Expected: PASS — the dispatch checks `is_remote_supported()` and rejects with a clear message.

**Step 3: Commit**

```bash
git add tests/cli_remote.rs
git commit -m "test(remote): verify unsupported commands rejected over SSH"
```

---

### Task 12: Run full test suite and verify no regressions

**Step 1: Run all tests**

Run: `cargo test`
Expected: All tests pass — existing local tests unchanged, new remote tests pass.

**Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

**Step 3: Commit (if any clippy fixes needed)**

```bash
git commit -m "fix: address clippy warnings from remote transport"
```

---

## Command coverage matrix

| Command | Remote support | Test coverage |
|---------|---------------|---------------|
| `start` | Yes — trailing args + quoting | Task 10 (3 tests: trailing args, quoting, child --host) |
| `status` | Yes — JSON passthrough | Task 5, Task 8 |
| `list` | Yes — JSON passthrough | Task 3, Task 4 |
| `log` | Yes — stdout streaming inherited | Implicit (stdio inherited) |
| `log -f` | Yes — streaming via inherited stdout | Implicit (stdio inherited) |
| `watch` | Yes — NDJSON streaming | Task 6 |
| `push` | Yes — stdin inherited | Implicit (stdio inherited) |
| `kill` | Yes — JSON passthrough | Implicit |
| `wait` | Yes — exit code preserved | Task 4 |
| `run` | **Rejected** — not remote-supported | Task 11 |
| `exec` | **Rejected** — not remote-supported | Task 11 |
| `wrap` | **Rejected** — not remote-supported | Task 11 |
| `prune` | **Rejected** — not remote-supported | Task 11 |

## Error classification tested

| Scenario | Expected behavior | Test |
|----------|-------------------|------|
| SSH exit 255 | `SshError::TransportFailed`, exit 1 | Task 4 |
| Remote tender exit code | Preserved exactly | Task 4 |
| ssh binary not on PATH | `SshError::SpawnFailed`, exit 1 | Task 7 |
| Remote stderr | Passed through to local stderr | Task 9 |
| Unsupported command | Clear error, exit 1, no SSH invoked | Task 11 |

## Quoting and arg fidelity tested

| Scenario | Expected behavior | Test |
|----------|-------------------|------|
| Args with spaces | `"hello world"` → quoted, round-trips through remote shell | Task 2 (unit), Task 10 |
| Shell metacharacters | `$HOME`, `foo;bar` → quoted, not expanded/split | Task 2 (unit), Task 10 |
| Child `--host` after `--` | Preserved — only top-level `--host` consumed by clap | Task 10 |
| `--namespace` forwarding | Included in remote command, `--host` absent | Task 8 |

## What is NOT in this plan

- **Windows remote hosts** — `shell_words::quote()` is POSIX-only. PowerShell and cmd.exe need a different quoting strategy. Deferred to a follow-on slice.
- `run` over SSH (needs script file transfer — follow-on)
- `exec` over SSH (needs interactive session support — follow-on)
- `prune` over SSH (safe to add later, same pattern)
- `wrap` and `_sidecar` over SSH (internal commands, not user-facing remotely)
- Automatic binary copy / version check
- Connection pooling or multiplexing
- Fanout orchestration
