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
| duckdb-exec | `duckdb-exec.md` | — |
| pty-session-mode | `pty-session-mode.md` | — |
| fleet-migration | `fleet-migration.md` | — |
| exec-annotation-ergonomics | `exec-annotation-ergonomics.md` | — |
| pty-automation | `pty-automation.md` | pty-session-mode |
| powershell-exec-framing | `powershell-exec-framing.md` | — |
| provenance-on-lifecycle-transitions | `provenance-on-lifecycle-transitions.md` | — |
| skill-claude-code | `skill-claude-code.md` | all other backlog items |

## completed/

30 completed plans. See `completed/` directory.

## specs/

Long-lived design documents (not queue items).

| File | Description |
|------|-------------|
| `tender-agent-process-sitter.md` | Full design spec |
| `decision-process-sitter-not-framework.md` | Decision: no native LLM protocol support |
