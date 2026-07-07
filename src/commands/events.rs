//! `tender events` — the protocol read surface, replay only in slice 1
//! (spec §5.1): read all segments of matching sessions, merge by
//! (ts, writer, seq), envelope NDJSON to stdout. Follow mode and cursors
//! arrive in slice 2.

use std::io::{self, Write};

use tender::events::{merge_key, read_session_events};
use tender::model::event::Event;
use tender::model::ids::{Namespace, SessionName};
use tender::session::{self, SessionRoot};

pub struct EventsOptions {
    pub namespace: Option<String>,
    pub sessions: Vec<String>,
    pub kinds: Vec<String>,
    pub sources: Vec<String>,
    pub strict: bool,
}

pub fn cmd_events(opts: EventsOptions) -> anyhow::Result<()> {
    let root = SessionRoot::default_path()?;
    let namespace_filter = opts.namespace.as_deref().map(Namespace::new).transpose()?;

    // Target sessions: the explicit --session list, else every session
    // (optionally namespace-filtered). Sessions in name order (spec §5.1).
    let targets: Vec<(Namespace, SessionName)> = if opts.sessions.is_empty() {
        session::list(&root, namespace_filter.as_ref())?
    } else {
        let mut targets = Vec::new();
        for spec in &opts.sessions {
            let (ns, name) = match spec.split_once('/') {
                Some((ns, name)) => (ns, name),
                None => ("default", spec.as_str()),
            };
            let namespace = Namespace::new(ns)?;
            if namespace_filter
                .as_ref()
                .is_some_and(|filter| filter != &namespace)
            {
                continue;
            }
            targets.push((namespace, SessionName::new(name)?));
        }
        targets
    };

    // A session with no events dir (pruned, pre-events, or never started)
    // contributes an empty log — replay is total over what exists.
    let mut events: Vec<Event> = Vec::new();
    let mut skipped = 0usize;
    for (namespace, name) in &targets {
        let session_dir = root.path().join(namespace.as_str()).join(name.as_str());
        let outcome = read_session_events(&session_dir)?;
        skipped += outcome.skipped;
        events.extend(outcome.events);
    }

    events.retain(|event| {
        (opts.kinds.is_empty()
            || opts
                .kinds
                .iter()
                .any(|prefix| event.kind.as_str().starts_with(prefix)))
            && (opts.sources.is_empty()
                || opts
                    .sources
                    .iter()
                    .any(|prefix| event.source.as_str().starts_with(prefix)))
    });
    events.sort_by_key(merge_key);

    let stdout = io::stdout().lock();
    let mut out = io::BufWriter::new(stdout);
    for event in &events {
        let line = serde_json::to_string(event)?;
        if writeln!(out, "{line}").is_err() {
            return Ok(()); // downstream pipe closed — normal for consumers
        }
    }
    let _ = out.flush();

    if opts.strict && skipped > 0 {
        eprintln!("tender events: {skipped} unparseable line(s) skipped");
        std::process::exit(65);
    }
    Ok(())
}
