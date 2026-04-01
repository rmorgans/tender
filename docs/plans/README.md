# Tender Plans

Spec: [tender-agent-process-sitter.md](specs/tender-agent-process-sitter.md)

Convention: see [CONVENTIONS.md](CONVENTIONS.md)

## active/ — Current Work

Filename prefix sets priority. `ls active/` is the ordered queue.

| ID | File | Depends On |
|----|------|------------|
| after-composition | `01-after-composition.md` | — |

## backlog/ — Future Work

| ID | File | Depends On |
|----|------|------------|
| exec | `exec.md` | wrap-annotation-ingestion (complete) |
| skill-claude-code | `skill-claude-code.md` | wrap-annotation-ingestion (complete) |
| fleet-migration | `fleet-migration.md` | remote-ssh-transport |
| remote-ssh-transport | `remote-ssh-transport.md` | — |
| pty-attach | `pty-attach.md` | — |

## completed/

21 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
