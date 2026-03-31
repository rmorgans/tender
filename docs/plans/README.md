# Tender Plans

Spec: [tender-agent-process-sitter.md](specs/tender-agent-process-sitter.md)

Convention: see [CONVENTIONS.md](CONVENTIONS.md)

## active/ — Current Work

Filename prefix sets priority. `ls active/` is the ordered queue.

| File | ID | Summary |
|------|----|---------|
| `00_windows-full-backend.md` | windows-full-backend | Sidecar, readiness, stdin, orphan kill |
| `10_run-shebang.md` | run-shebang | Supervised scripts via shebang |
| `20_wrap-annotation-ingestion.md` | wrap-annotation-ingestion | Streaming stdin tee, exec framing |

## backlog/ — Future Work

| ID | File | Depends On |
|----|------|------------|
| skill-and-migration | `skill-and-migration.md` | — |
| remote-ssh-transport | `remote-ssh-transport.md` | run-shebang, wrap-annotation-ingestion |
| pty-attach | `pty-attach.md` | run-shebang, wrap-annotation-ingestion |

## completed/

18 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
