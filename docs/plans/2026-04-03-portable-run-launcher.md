# Portable Run Launcher Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `tender run` cross-platform by replacing the implicit bash fallback with extension-based launcher resolution.

**Architecture:** Replace `resolve_shell_argv()` in `src/commands/run.rs` with a precedence chain: `--shell` > executable bit > extension mapping > shebang (Unix) > hard fail. Update `is_executable()` on Windows to recognise `.exe`/`.bat`/`.cmd`. Rewrite tests to use Python scripts for cross-platform coverage, keeping bash-specific tests under `#[cfg(unix)]`.

**Tech Stack:** Rust, cargo test, assert_cmd, predicates

---

### Task 1: Extension-to-launcher mapping and resolve_shell_argv rewrite

**Files:**
- Modify: `src/commands/run.rs:183-217` (resolve_shell_argv, is_executable)

**Step 1: Write the unit tests for launcher_argv_for_extension**

Add at the bottom of `src/commands/run.rs` inside a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_sh() {
        let argv = launcher_argv_for_extension("sh");
        assert_eq!(argv, Some(vec!["bash".to_string()]));
    }

    #[test]
    fn launcher_ps1() {
        let argv = launcher_argv_for_extension("ps1");
        assert_eq!(argv, Some(vec!["pwsh".to_string(), "-File".to_string()]));
    }

    #[test]
    fn launcher_py_platform() {
        let argv = launcher_argv_for_extension("py").unwrap();
        if cfg!(windows) {
            assert_eq!(argv, vec!["py", "-3"]);
        } else {
            assert_eq!(argv, vec!["python3"]);
        }
    }

    #[cfg(windows)]
    #[test]
    fn launcher_bat() {
        let argv = launcher_argv_for_extension("bat");
        assert_eq!(argv, Some(vec!["cmd".to_string(), "/c".to_string()]));
    }

    #[cfg(unix)]
    #[test]
    fn launcher_bat_unix() {
        assert!(launcher_argv_for_extension("bat").is_none());
    }

    #[test]
    fn launcher_unknown() {
        assert!(launcher_argv_for_extension("xyz").is_none());
    }

    #[test]
    fn launcher_rb() {
        let argv = launcher_argv_for_extension("rb");
        assert_eq!(argv, Some(vec!["ruby".to_string()]));
    }

    #[test]
    fn launcher_js() {
        let argv = launcher_argv_for_extension("js");
        assert_eq!(argv, Some(vec!["node".to_string()]));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib -- commands::run::tests`
Expected: FAIL — `launcher_argv_for_extension` not defined

**Step 3: Implement launcher_argv_for_extension**

Add above `resolve_shell_argv` in `src/commands/run.rs`:

```rust
/// Map a file extension to the interpreter argv prefix.
/// Returns None for unmapped extensions.
fn launcher_argv_for_extension(ext: &str) -> Option<Vec<String>> {
    let argv: &[&str] = match ext {
        "sh" => &["bash"],
        "ps1" => &["pwsh", "-File"],
        #[cfg(windows)]
        "bat" | "cmd" => &["cmd", "/c"],
        #[cfg(not(windows))]
        "bat" | "cmd" => return None,
        "py" => {
            if cfg!(windows) {
                &["py", "-3"]
            } else {
                &["python3"]
            }
        }
        "rb" => &["ruby"],
        "js" => &["node"],
        _ => return None,
    };
    Some(argv.iter().map(|s| s.to_string()).collect())
}
```

**Step 4: Run unit tests to verify they pass**

Run: `cargo test --lib -- commands::run::tests`
Expected: PASS

**Step 5: Rewrite resolve_shell_argv with the full precedence chain**

Replace the existing `resolve_shell_argv` and `is_executable`:

```rust
/// Resolve the child command argv based on --shell flag, executability,
/// extension mapping, or shebang.
///
/// Precedence: --shell > executable > extension mapping > shebang (Unix) > error.
fn resolve_shell_argv(
    shell: Option<&str>,
    script_str: &str,
    script_path: &Path,
    script_content: &str,
    args: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    let mut cmd = Vec::new();

    if let Some(sh) = shell {
        // 1. Explicit --shell override
        cmd.push(sh.to_string());
        cmd.push(script_str.to_string());
    } else if is_executable(script_path) {
        // 2. Directly executable
        cmd.push(script_str.to_string());
    } else if let Some(launcher) = script_path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(launcher_argv_for_extension)
    {
        // 3. Extension mapping
        cmd.extend(launcher);
        cmd.push(script_str.to_string());
    } else if let Some(interp) = parse_shebang(script_content) {
        // 4. Shebang (Unix only — Windows never has executable bit, so
        //    unmapped extensionless files always reach here or step 5)
        cmd.push(interp);
        cmd.push(script_str.to_string());
    } else {
        // 5. Hard fail
        anyhow::bail!(
            "cannot determine interpreter for '{}'\n  \
             hint: use --shell to specify the interpreter\n  \
             example: tender run --shell bash {}",
            script_path.display(),
            script_path.display(),
        );
    }

    cmd.extend(args);
    Ok(cmd)
}

/// Extract the interpreter from a shebang line.
/// Handles `#!/path/to/interp` and `#!/usr/bin/env interp`.
/// Returns None on Windows or if no shebang is found.
#[cfg(unix)]
fn parse_shebang(content: &str) -> Option<String> {
    let first_line = content.lines().next()?;
    let shebang = first_line.strip_prefix("#!")?;
    let shebang = shebang.trim();
    if shebang.is_empty() {
        return None;
    }
    // Handle "#!/usr/bin/env bash" → "bash"
    let parts: Vec<&str> = shebang.split_whitespace().collect();
    if parts.len() >= 2 && parts[0].ends_with("/env") {
        Some(parts[1].to_string())
    } else {
        Some(parts[0].to_string())
    }
}

#[cfg(windows)]
fn parse_shebang(_content: &str) -> Option<String> {
    None // Windows does not use shebangs
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    // On Windows, .exe/.bat/.cmd are directly executable by the OS.
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e, "exe" | "bat" | "cmd"))
        .unwrap_or(false)
}
```

**Step 6: Update the call site in cmd_run**

In `cmd_run`, the call at line 51 changes to pass `&content` and handle the Result:

```rust
    let cmd = resolve_shell_argv(shell.as_deref(), script_str, &script_path, &content, args)?;
```

Note: `content` is already read at line 33 for directive parsing.

**Step 7: Run all unit tests**

Run: `cargo test --lib -- commands::run`
Expected: PASS

**Step 8: Commit**

```
feat: add extension-based launcher resolution for tender run

Replace implicit bash fallback with explicit precedence chain:
--shell > executable > extension mapping > shebang > hard fail.

Supported extensions: .sh (bash), .ps1 (pwsh -File), .bat/.cmd
(cmd /c, Windows only), .py (py -3 / python3), .rb (ruby), .js (node).
```

---

### Task 2: Rewrite cli_run tests for cross-platform

**Files:**
- Modify: `tests/cli_run.rs` (rewrite most tests)

**Step 1: Add platform helpers and rewrite write_script**

At the top of `cli_run.rs`, add:

```rust
/// Write a Python script (cross-platform).
fn write_py_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let content = format!("import sys\n{body}\n");
    std::fs::write(&path, content).unwrap();
    path
}

/// Write a bash script (Unix only, sets +x).
#[cfg(unix)]
fn write_bash_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let content = format!("#!/bin/bash\n{body}\n");
    std::fs::write(&path, content).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}
```

**Step 2: Rewrite core tests using Python scripts**

Replace the bash-dependent foreground tests with Python equivalents. Key rewrites:

`run_blocks_and_returns_exit_code_zero`:
```rust
#[test]
fn run_blocks_and_returns_exit_code_zero() {
    let root = TempDir::new().unwrap();
    let script = write_py_script(root.path(), "hello.py", "print('hello-from-run')");

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello-from-run"));
}
```

`run_propagates_nonzero_exit_code`:
```rust
#[test]
fn run_propagates_nonzero_exit_code() {
    let root = TempDir::new().unwrap();
    let script = write_py_script(root.path(), "fail.py", "print('failing')\nsys.exit(7)");

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .code(7)
        .stdout(predicate::str::contains("failing"));
}
```

`run_passes_script_arguments`:
```rust
#[test]
fn run_passes_script_arguments() {
    let root = TempDir::new().unwrap();
    let script = write_py_script(root.path(), "args.py", "print('args:', ' '.join(sys.argv[1:]))");

    tender(&root)
        .args(["run", script.to_str().unwrap(), "foo", "bar"])
        .assert()
        .success()
        .stdout(predicate::str::contains("args: foo bar"));
}
```

`run_replace_reruns_script`:
```rust
#[test]
fn run_replace_reruns_script() {
    let root = TempDir::new().unwrap();
    let script = write_py_script(root.path(), "rerun.py", "print('rerun-output')");

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .success();

    tender(&root)
        .args(["run", "--replace", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("rerun-output"));
}
```

`run_foreground_overrides_detach_directive` — use Python with `#tender:` comments:
```rust
#[test]
fn run_foreground_overrides_detach_directive() {
    let root = TempDir::new().unwrap();
    let script = write_py_script(
        root.path(),
        "detachable.py",
        "#tender: detach\nprint('foreground-output')",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"Running\""));

    tender(&root)
        .args(["run", "--foreground", "--replace", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("foreground-output"));
}
```

`run_shell_flag_uses_specified_interpreter` — keep, but use Python as the shell:
```rust
#[test]
fn run_shell_flag_uses_specified_interpreter() {
    let root = TempDir::new().unwrap();
    let script_path = root.path().join("noshebang.txt");
    std::fs::write(&script_path, "print('shell-flag-works')\n").unwrap();

    let py = if cfg!(windows) { "py" } else { "python3" };
    tender(&root)
        .args(["run", "--shell", py, script_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("shell-flag-works"));
}
```

**Step 3: Keep detach/directive tests that already work cross-platform**

Tests that use `--detach` and only check JSON output don't need the script to actually run to completion, but they do need the process to start successfully. Convert these to `.py` scripts too for consistency, using `import time; time.sleep(30)` instead of `sleep 30`.

**Step 4: Move bash-specific tests under #[cfg(unix)]**

Any test that specifically tests bash shebang parsing or bash-only behavior:

```rust
#[cfg(unix)]
#[test]
fn run_shebang_resolution() {
    let root = TempDir::new().unwrap();
    let script = write_bash_script(root.path(), "shebang.sh", "echo shebang-works");

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("shebang-works"));
}
```

**Step 5: Add new tests for error path and extension mapping**

```rust
#[test]
fn run_unknown_extension_fails_with_hint() {
    let root = TempDir::new().unwrap();
    let script_path = root.path().join("mystery.xyz");
    std::fs::write(&script_path, "some content\n").unwrap();

    tender(&root)
        .args(["run", script_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot determine interpreter"))
        .stderr(predicate::str::contains("--shell"));
}

#[test]
fn run_py_extension_uses_python() {
    let root = TempDir::new().unwrap();
    let script = write_py_script(root.path(), "check.py", "print('py-ext-works')");

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("py-ext-works"));
}
```

**Step 6: Run full test suite on local platform**

Run: `cargo test --test cli_run`
Expected: PASS

**Step 7: Commit**

```
test: rewrite cli_run tests for cross-platform support

Replace bash-dependent tests with Python equivalents using extension-
based launcher resolution. Bash-only tests moved under #[cfg(unix)].
New tests for unknown extension error and --shell hint.
```

---

### Task 3: Windows validation and cleanup

**Step 1: Push and run on Windows**

```bash
git push
ssh rick-windows 'cd C:\Users\rick\tender; git pull; cargo test --test cli_run 2>&1'
```

Expected: all tests pass (Python via `py -3`, bash tests skipped)

**Step 2: Run full suite on both platforms**

Local: `cargo test`
Windows: `ssh rick-windows 'cd C:\Users\rick\tender; cargo test 2>&1'`

**Step 3: If any test fails, fix and re-run**

Common issues:
- Python directive comment format (`#tender:` in `.py` uses `#` which is a valid Python comment — this should work)
- Windows `py` launcher not finding Python 3

**Step 4: Archive backlog plan**

Move `docs/plans/backlog/portable-run-launcher.md` to `docs/plans/completed/2026-04-03-portable-run-launcher.md` with a status header.

**Step 5: Final commit**

```
docs: archive portable-run-launcher as completed
```
