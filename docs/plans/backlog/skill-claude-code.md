---
id: skill-claude-code
depends_on:
  - wrap-annotation-ingestion
links: []
---

# Claude Code Skill for Tender

Write a Claude Code skill that teaches agents how to use tender for supervised process execution.

## Scope

A `.claude/skills/` document that covers:

- `tender start` — launch supervised processes
- `tender status` / `tender list` — check on sessions
- `tender log` / `tender watch` — observe output and events
- `tender push` — send stdin to running sessions
- `tender kill` — stop sessions (graceful and forced)
- `tender wrap` — annotate hook command execution with provenance
- `tender run` — supervised script execution (once implemented)
- Session naming, namespaces, idempotency semantics
- `--timeout`, `--on-exit`, `--replace` composition flags
- Windows differences (named pipes vs FIFOs, session storage paths)

## Deliverables

1. Skill file: `tender.md` (or `tender/SKILL.md` if multi-file)
2. Trigger patterns: when user says "start a process", "supervise", "run in background", "check on job", etc.
3. Examples showing common workflows: start + wait, start + watch, wrap for hooks
4. Error handling guidance: what exit codes mean, how to recover from sidecar_lost

## Depends On

`wrap-annotation-ingestion` (complete) — the skill must document `tender wrap` for hook integration, which is the primary Claude Code integration path.

## Not Blocked By

- `exec` — the skill can document start/push/wrap without exec
- `run-shebang` — the skill can be updated when run lands
- `remote-ssh-transport` — remote usage is additive, not foundational

## Notes

Consider promoting to active once `run-shebang` is complete — the skill is high-leverage and does not need to wait for remote or PTY features.

> **Needs expansion before promotion:** Add concrete skill content outline, example trigger/response pairs, and decide on skill file structure.
