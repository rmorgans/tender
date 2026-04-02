# Tender Plans

Spec: [tender-agent-process-sitter.md](specs/tender-agent-process-sitter.md)

Convention: see [CONVENTIONS.md](CONVENTIONS.md)

## active/ — Current Work

Filename prefix sets priority. `ls active/` is the ordered queue.

No active plans. See backlog for next candidates.

## backlog/ — Future Work

| ID | File | Depends On |
|----|------|------------|
| agent-exec-spike | `agent-exec-spike.md` | — |
| explicit-exec-targets | `explicit-exec-targets.md` | — |
| duckdb-exec | `duckdb-exec.md` | explicit-exec-targets |
| python-repl-exec | `python-repl-exec.md` | explicit-exec-targets |
| pty-session-mode | `pty-session-mode.md` | — |
| fleet-migration | `fleet-migration.md` | remote-ssh-transport (complete) |
| pty-automation | `pty-automation.md` | pty-session-mode |
| skill-claude-code | `skill-claude-code.md` | all other backlog items |

## completed/

27 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
| `decision-process-sitter-not-framework.md` | Decision: no native LLM protocol support |
