---
id: backlog-04
title: "PTY Attach — Human Escape Hatch"
created: 2026-03-17
depends_on:
  - frontlog-01
  - frontlog-02
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

- Phase 2B frontlog complete (session model must be stable)
- Remote SSH transport is NOT a hard dependency — PTY attach is useful locally ("my agent is stuck, let me take over"). Remote combination is nice but not required.

## Notes

Original design spec Phase 5. Lowest priority — agents are the primary consumer.
