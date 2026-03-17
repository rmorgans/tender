use tender::model::ids::SessionName;
use tender::platform::{Current, Platform};
use tender::session::{self, SessionRoot};

pub fn cmd_kill(name: &str, force: bool) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    // Session doesn't exist -- idempotent success
    let session = match session::open(&root, &session_name)? {
        Some(s) => s,
        None => {
            println!(
                "{}",
                serde_json::json!({"session": name, "result": "not_found"})
            );
            return Ok(());
        }
    };

    let meta = session::read_meta(&session)?;

    // Already terminal -- idempotent success
    if meta.status().is_terminal() {
        let json = serde_json::to_string_pretty(&meta)?;
        println!("{json}");
        return Ok(());
    }

    // Get child identity from Running state
    let child = match meta.status().child() {
        Some(c) => *c,
        None => {
            // Starting state with no child -- nothing to kill
            println!(
                "{}",
                serde_json::json!({"session": name, "result": "no_child"})
            );
            return Ok(());
        }
    };

    // Write kill_forced marker before force-killing so sidecar can detect it
    let kill_forced_path = if force {
        let p = session.path().join("kill_forced");
        let _ = std::fs::write(&p, "");
        Some(p)
    } else {
        None
    };

    // Kill the child's process group. Verifies identity first.
    if let Err(e) = Current::kill_orphan(&child, force) {
        // Clean up marker on error -- don't leave a stale marker that could
        // mislabel a later exit as KilledForced
        if let Some(ref p) = kill_forced_path {
            let _ = std::fs::remove_file(p);
        }
        return Err(e.into());
    }

    // Wait briefly for sidecar to write terminal state, then re-read
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(m) = session::read_meta(&session) {
            if m.status().is_terminal() {
                let json = serde_json::to_string_pretty(&m)?;
                println!("{json}");
                return Ok(());
            }
        }
    }

    // Sidecar didn't write terminal state in time -- report what we know
    println!(
        "{}",
        serde_json::json!({"session": name, "result": "kill_sent", "force": force})
    );
    Ok(())
}
