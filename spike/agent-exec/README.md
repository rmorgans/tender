# Agent Orchestration Spike

Tests whether tender's existing `--after` dependency chains work for orchestrating one-shot coding agents (like `claude -p` and `codex exec`).

**Verdict: It works.** Tender's existing primitives are sufficient for multi-agent pipelines with no Rust changes needed.

## Scripts

| Script | What it tests | Result |
|--------|--------------|--------|
| `01-basic-chain.sh` | Linear 3-session chain: research -> impl -> review | PASS |
| `02-parallel-fan-out.sh` | Two parallel sessions, one fan-in waiting for both | PASS |
| `03-failure-propagation.sh` | Middle session fails, downstream behavior | PASS (documented) |
| `04-real-agent-chain.sh` | Template for real claude/codex agents (echo stand-ins by default) | PASS |

## How to run

```bash
# Build tender first
cargo build

# Run all spike scripts
./spike/agent-exec/01-basic-chain.sh
./spike/agent-exec/02-parallel-fan-out.sh
./spike/agent-exec/03-failure-propagation.sh
./spike/agent-exec/04-real-agent-chain.sh

# Run 04 with real agents (needs API keys)
USE_REAL_AGENTS=1 ./spike/agent-exec/04-real-agent-chain.sh
```

Each script uses a unique namespace (`spike-*-$$`) to avoid collisions and cleans up after itself.

## Findings

### 1. `--after` chaining works reliably

Linear chains (A -> B -> C) execute in strict order. Each session waits for its dependency to reach terminal state before starting. File-based communication between sessions works perfectly when using `--cwd` to share a working directory.

### 2. Parallel fan-out + fan-in works

Multiple `--after` flags on a single session create an AND-join: `--after auth --after db` waits for BOTH to complete. The two upstream sessions run in parallel as expected.

### 3. Failure propagation is well-designed

When a dependency exits non-zero:
- **Default behavior:** Downstream sessions enter `DependencyFailed` status with `dep_reason: "Failed"` and a warning message. The child process is never started. This is the safe default for agent pipelines.
- **`--any-exit` flag:** Downstream proceeds regardless of upstream exit code. Useful for "best-effort" pipelines where a review should run even if implementation partially failed.

### 4. `--cwd` enables shared workdirs

The `--cwd` flag lets all sessions in a pipeline share a working directory, which is the natural communication channel for coding agents (they read/write files). This is simpler than piping logs between sessions.

### 5. `tender log -r` gives clean output

The `-r` (raw) flag strips the JSONL envelope, giving plain text output suitable for feeding into downstream agents or human review.

### 6. Status reporting is clear

Session status JSON includes:
- `status: "Exited"` with `reason: "ExitedOk"` or `reason: "ExitedError"` + `code`
- `status: "DependencyFailed"` with `dep_reason` and `warnings`
- `status: "Starting"` while waiting for dependencies
- Timestamps for start/end

## What's missing (potential improvements)

1. **No `tender pipeline` command** -- Declaring a full pipeline requires multiple `tender start` calls. A declarative pipeline spec (YAML/TOML) could simplify complex DAGs, but is not needed for the basic use case.

2. **No built-in log forwarding** -- A downstream agent cannot easily read the upstream's tender log as part of its prompt without a wrapper script doing `tender log upstream -r`. An `--inject-dep-logs` flag could auto-prepend dependency output.

3. **`tender wait` exit code** -- `tender wait` returns exit code 42 when any waited session fails (not the child's actual exit code). This is usable for scripting (`|| true` to ignore), but forwarding the child's actual exit code could be more useful.

4. **No pipeline-level status** -- `tender list --namespace X` shows sessions, but there's no single "pipeline succeeded/failed" summary. Not blocking, but would be convenient.

## Answers to spike questions

1. **Does `--after` chaining work reliably with one-shot agents?** Yes.
2. **Is file-based communication sufficient?** Yes, using `--cwd` for shared workdirs. Agents naturally read/write files.
3. **What timeout values are practical?** For real agents, 300s (5 min) per session is a reasonable starting point. Use `--timeout`.
4. **Does parallel fan-out + fan-in work?** Yes, with multiple `--after` flags.
5. **What's missing?** Nothing blocking. See improvements list above.
