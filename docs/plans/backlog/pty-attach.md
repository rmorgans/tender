---
id: pty-attach
depends_on: []
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

No technical blockers. The previous `depends_on: [run-shebang, wrap-annotation-ingestion]` was sequencing preference, not a real dependency. PTY attach is useful locally without either feature ("my agent is stuck, let me take over").

## Notes

Lowest priority — agents are the primary consumer. Windows ConPTY implementation is now feasible (the platform trait exists and works as of 2026-04-01).

> **Needs full design expansion before promotion to active.** This is four bullet points for what is one of the hardest features in the roadmap. Before promotion, expand to cover: PTY allocation in the sidecar, interaction with output.log capture, `PtySession` trait on both platforms (forkpty / ConPTY), attach/detach protocol, terminal size propagation, and what happens when multiple clients try to attach.
