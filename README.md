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

> `tender exec` takes argv, not a shell snippet. For multi-step shell commands, use separate `exec` calls or wrap explicitly with `bash -c '...'`.

The same model works for REPLs — Python, IPython, PowerShell, DuckDB — not just shells:

```bash
tender start --stdin py -- python3 -i                 # auto-inferred: python-repl
tender exec  py -- 'import pandas as pd; df = pd.read_csv("data.csv")'
tender exec  py -- 'print(df.describe())'             # df still loaded
```

In-memory REPL state — imported modules, loaded DataFrames, opened connections — survives across every `exec`. A DuckDB analyst session, a PyTorch notebook-equivalent, an IPython exploration — all as durable as a shell.

<details>
<summary><b>DuckDB</b> — persistent SQL session with structured output</summary>

```bash
tender start --stdin ddb -- duckdb :memory:          # auto-inferred: duckdb
tender exec  ddb -- "CREATE TABLE t AS SELECT range AS id, range * 2 AS val FROM range(5);"
tender exec  ddb -- "SELECT count(*), sum(val) FROM t;"
# → stdout: [{"count_star()":5,"sum(val)":"20"}]
```

Tables, views, attached databases, loaded extensions — all persist across execs. Output arrives as structured JSON rows, not a formatted table, so agents parse it directly.

</details>

<details>
<summary><b>IPython</b> — rich Python REPL with persistent namespace</summary>

```bash
tender start --stdin ipy --exec-target python-repl -- ipython --no-banner --no-confirm-exit
tender exec  ipy -- 'from statistics import mean; xs = list(range(10))'
tender exec  ipy -- 'print(mean(xs), sum(xs))'
# → stdout: 4.5 45
```

Imports, function definitions, large loaded datasets stay in memory. Start with `python3 -i` for stdlib Python, or `ipython` for the richer REPL — both use the same `python-repl` exec target and side-channel result protocol.

</details>

<details>
<summary><b>PowerShell</b> — Windows-native and cross-platform via <code>pwsh</code></summary>

```bash
tender start --stdin ps -- pwsh -NoLogo                # auto-inferred: powershell
tender exec  ps -- '$items = @(1..10) | ForEach-Object { $_ * 2 }'
tender exec  ps -- '$items | Measure-Object -Sum | Select-Object Count, Sum'
```

PowerShell variables, imported modules, and loaded `.ps1` dot-sourced state persist. The exec protocol uses PowerShell-specific sentinel framing. Works identically on Windows (`powershell.exe` / `pwsh`) and cross-platform (`pwsh`).

</details>

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

- Architecture overview: [docs/architecture/README.md](docs/architecture/README.md)
- Design spec: [docs/plans/specs/tender-agent-process-sitter.md](docs/plans/specs/tender-agent-process-sitter.md)
- Planning index: [docs/plans/README.md](docs/plans/README.md)
