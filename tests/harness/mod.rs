#[allow(unused_imports)]
use assert_cmd::Command;
#[allow(unused_imports)]
use tempfile::TempDir;

/// Create a `tender` command rooted in a temp HOME.
#[allow(dead_code)]
pub fn tender(root: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("tender").expect("tender binary not found");
    cmd.env("HOME", root.path());
    // On Windows, ensure Git-for-Windows coreutils (echo, sleep, true, cat)
    // are on PATH so tests can spawn Unix-style commands.
    #[cfg(windows)]
    {
        let git_usr_bin = std::path::Path::new(r"C:\Program Files\Git\usr\bin");
        if git_usr_bin.exists() {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{};{path}", git_usr_bin.display()));
        }
    }
    cmd
}

/// Wait for meta.json to show Running state on disk.
#[allow(dead_code)]
pub fn wait_running(root: &TempDir, session: &str) {
    let path = root
        .path()
        .join(format!(".tender/sessions/default/{session}/meta.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                if meta["status"].as_str() == Some("Running") {
                    return;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for Running state in {session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Wait for meta.json to reach any terminal state on disk.
#[allow(dead_code)]
pub fn wait_terminal(root: &TempDir, session: &str) -> serde_json::Value {
    let path = root
        .path()
        .join(format!(".tender/sessions/default/{session}/meta.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                let status = meta["status"].as_str().unwrap_or("");
                if status != "Starting" && status != "Running" {
                    return meta;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for terminal state in {session}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
