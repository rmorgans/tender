# Tender Plans

Reference: [tender-agent-process-sitter.md](2026-03-16-tender-agent-process-sitter.md) — full design spec

## completed/

- [phase1-implementation.md](completed/2026-03-16-phase1-implementation.md) — 8 TDD slices: model, session, sidecar, CLI, log, push, wait, replace (56 commits, 178 tests)
- [slice6-log-query.md](completed/2026-03-16-slice6-log-query.md) — tail/follow/grep/since/raw
- [slice7-stdin-push.md](completed/2026-03-16-slice7-stdin-push.md) — FIFO transport, multiple sequential pushes
- [slice8a-wait-reconcile-replace.md](completed/2026-03-17-slice8a-wait-reconcile-replace.md) — wait, sidecar-lost reconciliation, idempotent start, --replace
- [slice8b-hardening.md](completed/2026-03-17-slice8b-hardening.md) — EpochTimestamp, warnings, timeout, KilledForced, typed readiness
- [phase1.5-refactor.md](completed/2026-03-17-phase1.5-refactor.md) — extract helpers, .context() errors
- [phase1.6-polish.md](completed/2026-03-17-phase1.6-polish.md) — ProcessIdentity breadcrumb, generation increment
- [phase2a-platform-trait.md](completed/2026-03-17-phase2a-platform-trait.md) — Platform trait seam, Unix impl, Windows skeleton

## frontlog/ — Ordered Queue

1. [phase2b-cmux-integration.md](frontlog/2026-03-28-phase2b-cmux-integration.md) — v0.2.0: launch fidelity, namespace, on-exit, watch
2. [tender-run-shebang.md](frontlog/tender-run-shebang.md) — `#!/usr/bin/env -S tender run` supervised scripts (sugar over start, needs slices 0-2)
3. [gc-prune.md](frontlog/gc-prune.md) — session cleanup (needed before any real multi-workspace trial)
4. [wrap-annotation-ingestion.md](frontlog/wrap-annotation-ingestion.md) — transparent hook tapping + exec (completes the cmux event story)

## backlog/

**Independent — can start anytime:**
- [windows-full-backend.md](backlog/windows-full-backend.md) — complete Windows platform (CreateProcess, Job Objects, named pipes)
- [skill-and-migration.md](backlog/skill-and-migration.md) — Claude Code skill (local), atch migration guide (fleet needs remote)

**Depends on frontlog completing:**
- [remote-ssh-transport.md](backlog/remote-ssh-transport.md) — semantic remote backend, SSH transport, broker/relay deferred
- [pty-attach.md](backlog/pty-attach.md) — human escape hatch (forkpty/ConPTY, attach/detach)
