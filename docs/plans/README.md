# Tender Plans

Spec: [tender-agent-process-sitter.md](specs/tender-agent-process-sitter.md)

Convention: see [CONVENTIONS.md](CONVENTIONS.md)

## active/ — Current Work

Filename prefix sets priority. `ls active/` is the ordered queue.

| ID | File | Depends On |
|----|------|------------|
| log-jsonl-output | `00_log-jsonl-output.md` | — |

## backlog/ — Future Work

| ID | File | Depends On |
|----|------|------------|
| wait-multiple | `wait-multiple.md` | — |
| explicit-exec-targets | `explicit-exec-targets.md` | — |
| pty-session-mode | `pty-session-mode.md` | — |
| fleet-migration | `fleet-migration.md` | remote-ssh-transport (complete) |
| pty-automation | `pty-automation.md` | pty-session-mode |
| skill-claude-code | `skill-claude-code.md` | all other backlog items |

## completed/

25 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
