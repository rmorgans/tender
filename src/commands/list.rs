use tender::session::{self, SessionRoot};

pub fn cmd_list() -> anyhow::Result<()> {
    let root = SessionRoot::default_path()?;
    let sessions = session::list(&root)?;

    let names: Vec<&str> = sessions.iter().map(|s| s.as_str()).collect();
    let json = serde_json::to_string_pretty(&names)?;
    println!("{json}");

    Ok(())
}
