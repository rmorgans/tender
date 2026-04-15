# Tender

Coding agents run every shell command in a fresh subprocess. Shell state — cwd, activated venvs, running REPLs, dev servers — dies between tool calls. Logs live in the agent's context and vanish on crash or compaction. Long-running processes hit timeouts and lose partial output. Interactive prompts deadlock because backgrounded processes can't accept stdin.

Tender fixes that. It supervises shells and long-lived processes as durable, named sessions. The agent's one-shot CLI calls become thin clients against a supervisor that owns the process. State persists across tool calls. Logs survive on disk. Multiple agents — Claude, Codex, or anything else with shell access — can share the same session because no agent owns it.

## A Concrete Day One

```bash
tender start --stdin dev -- bash          # supervised shell, durable
tender exec  dev -- cd repo
tender exec  dev -- . .venv/bin/activate
tender exec  dev -- pytest -x             # cwd + venv still active
tender log   dev --tail 50                # on-disk, survives crashes
printf 'y\n' | tender push dev            # answer an interactive prompt
```

Every call above is a separate subprocess from the agent's side. The *shell* lives inside Tender and outlives all of them.

## The Core Idea

```text
Tender = stateless CLI + durable session record + per-session supervisor + OS-native kill/wait
```

The sidecar is the supervisor. The CLI is a transactional client. `meta.json` and `output.log` are the durable truth for a run.

## What Tender Is Not

- **Not an AI agent.** It does not plan, reason, or choose tools.
- **Not an LLM orchestration framework.** No model routing, token tracking, or prompt handling.
- **Not a terminal UI or tmux replacement.** It supervises sessions; render or attach with whatever you want.
- **Not a sandbox or security boundary.** Isolation stays with Claude, Codex, Docker, SSH.
- **Not for every shell command.** One-shot subprocesses don't need a supervisor — keep using plain Bash.
- **Not a second remote lifecycle.** SSH is just another access path to the same session model.

## Current Surface

Core commands today include:

- `start`
- `run`
- `exec`
- `push`
- `attach`
- `log`
- `status`
- `wait`
- `watch`
- `kill`
- `wrap`

These operate on named sessions and stable run identities rather than on raw process trees.

## Design Direction

Tender has one lifecycle model and multiple access paths.

- local execution calls Tender core directly
- remote execution should be the same semantic contract over SSH, not a separate system
- pipe `start --stdin` + `exec` is the default agent lane for persistent shells
- PTY support is secondary and should stay a distinct execution lane from structured non-PTY `exec`

That makes Tender a good foundation for:

- agent supervisors
- hook-driven tooling
- remote orchestration
- higher-level frontends that need durable execution, logs, and control without owning process lifecycle themselves

## Docs

- Design spec: [docs/plans/specs/tender-agent-process-sitter.md](docs/plans/specs/tender-agent-process-sitter.md)
- Planning index: [docs/plans/README.md](docs/plans/README.md)
