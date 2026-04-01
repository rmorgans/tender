# Tender Plans

Spec: [tender-agent-process-sitter.md](specs/tender-agent-process-sitter.md)

Convention: see [CONVENTIONS.md](CONVENTIONS.md)

## active/ — Current Work

Filename prefix sets priority. `ls active/` is the ordered queue.

No active plans. See backlog for next candidates.

## backlog/ — Future Work

| ID | File | Depends On |
|----|------|------------|
| remote-ssh-transport | `remote-ssh-transport.md` | — |
| pty-session-mode | `pty-session-mode.md` | — |
| skill-claude-code | `skill-claude-code.md` | wrap-annotation-ingestion (complete) |
| exec-windows-shells | `exec-windows-shells.md` | exec (complete) |
| pty-automation | `pty-automation.md` | pty-session-mode |
| fleet-migration | `fleet-migration.md` | remote-ssh-transport |

## completed/

23 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
