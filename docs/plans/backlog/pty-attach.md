---
id: pty-attach
title: "PTY Attach — Human Escape Hatch"
created: 2026-03-17
closed:
depends_on:
  - run-shebang
  - wrap-annotation-ingestion
links: []
---

# PTY Attach — Human Escape Hatch

Let humans take over when agents can't handle it.

## Scope

- PTY support (Unix: forkpty, Windows: ConPTY)
- `tender attach <session>` command
- Detach key handling
- Requires process started with PTY mode

## Depends On

See frontmatter `depends_on`. Remote SSH transport (`remote-ssh-transport`) is NOT a hard dependency — PTY attach is useful locally ("my agent is stuck, let me take over").

## Notes

Lowest priority — agents are the primary consumer.
