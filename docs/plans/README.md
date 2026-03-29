# Tender Plans

Reference: [tender-agent-process-sitter.md](2026-03-16-tender-agent-process-sitter.md) — full design spec

Convention: see `CONVENTIONS.md` (shared with edge-platform, machine-learning, starling-edr).

## frontlog/ — Ordered Queue

| # | ID | Plan |
|---|-----|------|
| 01 | `run-shebang` | [tender-run-shebang.md](frontlog/tender-run-shebang.md) — supervised scripts via shebang |
| 02 | `wrap-annotation-ingestion` | [wrap-annotation-ingestion.md](frontlog/wrap-annotation-ingestion.md) — streaming stdin tee, exec framing |

## backlog/

| ID | Plan | Depends On |
|----|------|------------|
| `windows-full-backend` | [windows-full-backend.md](backlog/windows-full-backend.md) — sidecar, readiness, stdin transport | — |
| `skill-and-migration` | [skill-and-migration.md](backlog/skill-and-migration.md) — Claude Code skill, atch migration | — |
| `remote-ssh-transport` | [remote-ssh-transport.md](backlog/remote-ssh-transport.md) — SSH transport, remote backend | run-shebang, wrap-annotation-ingestion |
| `pty-attach` | [pty-attach.md](backlog/pty-attach.md) — forkpty/ConPTY, attach/detach | run-shebang, wrap-annotation-ingestion |

## completed/

17 completed plans (Phase 1 through wrap platform refactor). See `completed/` directory.
