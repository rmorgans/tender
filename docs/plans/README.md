# Tender Plans

Reference: [tender-agent-process-sitter.md](2026-03-16-tender-agent-process-sitter.md) — full design spec

## Convention

All frontlog and backlog items use YAML frontmatter with stable IDs:

```yaml
---
id: frontlog-01          # stable, survives reordering and completion
title: "Short title"
created: 2026-03-17
depends_on:              # list of IDs from frontlog or backlog
  - frontlog-01
links:                   # relative paths to related docs
  - ../design/foo.md
---
```

IDs are `frontlog-NN` or `backlog-NN`. Completed items keep their original ID.
Dependencies reference IDs, not filenames or prose descriptions.

## frontlog/ — Ordered Queue

| ID | Plan | Status |
|----|------|--------|
| frontlog-01 | [tender-run-shebang.md](frontlog/tender-run-shebang.md) — supervised scripts via shebang | Ready |
| frontlog-02 | [wrap-annotation-ingestion.md](frontlog/wrap-annotation-ingestion.md) — streaming stdin tee, exec framing | Ready |

## backlog/

| ID | Plan | Depends On |
|----|------|------------|
| backlog-01 | [windows-full-backend.md](backlog/windows-full-backend.md) — sidecar, readiness, stdin transport | — |
| backlog-02 | [skill-and-migration.md](backlog/skill-and-migration.md) — Claude Code skill, atch migration | — |
| backlog-03 | [remote-ssh-transport.md](backlog/remote-ssh-transport.md) — SSH transport, remote backend | frontlog-01, frontlog-02 |
| backlog-04 | [pty-attach.md](backlog/pty-attach.md) — forkpty/ConPTY, attach/detach | frontlog-01, frontlog-02 |

## completed/

16 completed plans (Phase 1 through wrap platform refactor). See `completed/` directory.
