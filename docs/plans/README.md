# Tender Plans

Spec: [tender-agent-process-sitter.md](specs/tender-agent-process-sitter.md)

Convention: see [CONVENTIONS.md](CONVENTIONS.md)

## active/ — Current Work

Filename prefix sets priority. `ls active/` is the ordered queue.

| ID | File | Depends On |
|----|------|------------|
| exec | `01-exec.md` | wrap-annotation-ingestion (complete) |

## backlog/ — Future Work

| ID | File | Depends On |
|----|------|------------|
| skill-claude-code | `skill-claude-code.md` | wrap-annotation-ingestion (complete) |
| fleet-migration | `fleet-migration.md` | remote-ssh-transport |
| remote-ssh-transport | `remote-ssh-transport.md` | — |
| pty-session-mode | `pty-session-mode.md` | — |
| pty-automation | `pty-automation.md` | pty-session-mode |

## completed/

22 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
