mod harness;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn tender(root: &TempDir) -> Command {
    harness::tender(root)
}

fn write_script(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn run_blocks_and_returns_exit_code_zero() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "hello.sh",
        "#!/bin/bash\necho hello-from-run\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello-from-run"));
}

#[test]
fn run_propagates_nonzero_exit_code() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "fail.sh",
        "#!/bin/bash\necho failing\nexit 7\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .code(7)
        .stdout(predicate::str::contains("failing"));
}

#[test]
fn run_detach_returns_immediately_with_json() {
    let root = TempDir::new().unwrap();
    let script = write_script(root.path(), "slow.sh", "#!/bin/bash\nsleep 30\n");

    tender(&root)
        .args(["run", "--detach", script.to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"Running\""));
}

#[test]
fn run_directives_map_to_launch_spec() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "directives.sh",
        "\
#!/bin/bash
#tender: namespace=test-ns
#tender: timeout=999
#tender: session=my-session
#tender: detach

sleep 30
",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"session\": \"my-session\""))
        .stdout(predicate::str::contains("\"timeout_s\": 999"))
        .stdout(predicate::str::contains("\"namespace\": \"test-ns\""));
}

#[test]
fn run_cli_flags_override_directives() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "override.sh",
        "\
#!/bin/bash
#tender: namespace=directive-ns
#tender: timeout=999
#tender: detach

sleep 30
",
    );

    // CLI --namespace should override the directive.
    tender(&root)
        .args([
            "run",
            "--namespace",
            "cli-ns",
            "--timeout",
            "42",
            script.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"namespace\": \"cli-ns\""))
        .stdout(predicate::str::contains("\"timeout_s\": 42"));
}

#[test]
fn run_nonexistent_script_fails() {
    let root = TempDir::new().unwrap();

    tender(&root)
        .args(["run", "/nonexistent/path/script.sh"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("script not found"));
}

#[test]
fn run_session_name_from_filename() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "my-build.sh",
        "#!/bin/bash\n#tender: detach\nsleep 30\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"session\": \"my-build\""));
}

#[test]
fn run_session_directive_overrides_filename() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "build.sh",
        "#!/bin/bash\n#tender: session=custom-name\n#tender: detach\nsleep 30\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"session\": \"custom-name\""));
}

#[test]
fn run_shell_flag_uses_specified_interpreter() {
    let root = TempDir::new().unwrap();
    // Write a script that is NOT executable and has no shebang.
    // With --shell bash, it should still run.
    let script = write_script(root.path(), "noshebang.txt", "echo shell-flag-works\n");
    // Remove execute permission.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    tender(&root)
        .args(["run", "--shell", "bash", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("shell-flag-works"));
}

#[test]
fn run_foreground_overrides_detach_directive() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "detachable.sh",
        "#!/bin/bash\n#tender: detach\necho foreground-output\n",
    );

    // Without --foreground, the directive causes detach (JSON output).
    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"Running\""));

    // With --foreground, the directive is overridden — we see the script output.
    tender(&root)
        .args(["run", "--foreground", "--replace", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("foreground-output"));
}

#[test]
fn run_invalid_cli_namespace_fails() {
    let root = TempDir::new().unwrap();
    let script = write_script(root.path(), "good.sh", "#!/bin/bash\necho hi\n");

    tender(&root)
        .args(["run", "--namespace", "bad name", script.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("whitespace"));
}

#[test]
fn run_invalid_namespace_directive_fails() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "badns.sh",
        "#!/bin/bash\n#tender: namespace=bad name\necho hi\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid namespace"));
}

#[test]
fn run_invalid_session_directive_fails() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "badsession.sh",
        "#!/bin/bash\n#tender: session=my.bad.name\necho hi\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid session name"));
}

#[test]
fn run_unknown_directive_errors() {
    let root = TempDir::new().unwrap();
    let script = write_script(
        root.path(),
        "bad.sh",
        "#!/bin/bash\n#tender: timout=30\necho hi\n",
    );

    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown directive"));
}

#[test]
fn run_passes_script_arguments() {
    let root = TempDir::new().unwrap();
    let script = write_script(root.path(), "args.sh", "#!/bin/bash\necho \"args: $@\"\n");

    tender(&root)
        .args(["run", script.to_str().unwrap(), "foo", "bar"])
        .assert()
        .success()
        .stdout(predicate::str::contains("args: foo bar"));
}

#[test]
fn run_replace_reruns_script() {
    let root = TempDir::new().unwrap();
    let script = write_script(root.path(), "rerun.sh", "#!/bin/bash\necho rerun-output\n");

    // First run
    tender(&root)
        .args(["run", script.to_str().unwrap()])
        .assert()
        .success();

    // Second run with --replace
    tender(&root)
        .args(["run", "--replace", script.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("rerun-output"));
}
