---
id: wait-multiple
depends_on: []
links: []
---

# Wait for Multiple Sessions

`tender wait` accepts multiple session names and blocks until all (or any) reach terminal state.

## Goal

Agent orchestrators running parallel tasks should not need to poll `tender status` in a loop for each session. One `wait` call handles fan-out.

## Why

The common agent pattern is:

```bash
tender start --namespace ci --session test-unit -- pytest tests/unit/
tender start --namespace ci --session test-integration -- pytest tests/integration/
tender start --namespace ci --session lint -- ruff check .

# Wait for all three
tender wait --namespace ci test-unit test-integration lint
```

Today this requires three separate `tender wait` calls (sequential) or polling `tender status` for each (wasteful). Neither is good.

## CLI Surface

```bash
# Wait for ALL to reach terminal state (default)
tender wait [--namespace NS] [--timeout DURATION] SESSION [SESSION...]

# Wait for ANY one to reach terminal state
tender wait --any [--namespace NS] [--timeout DURATION] SESSION [SESSION...]
```

Multiple positional arguments. `--any` returns as soon as the first session terminates.

## Output

JSON array of meta snapshots for all waited sessions (consistent with structured-output-only rule):

```json
[
  {"session":"test-unit","status":"Exited","exit_code":0,...},
  {"session":"test-integration","status":"Exited","exit_code":0,...},
  {"session":"lint","status":"Exited","exit_code":1,...}
]
```

With `--any`, the array contains only the session(s) that reached terminal state.

## Exit Code

Multi-session exit code picks the most severe failure in the set:

- `0`: all sessions exited successfully
- `2`: at least one spawn failure (most severe — process never started)
- `3`: at least one sidecar lost (supervision crashed)
- `4`: at least one dependency failure (upstream failure)
- `42`: at least one non-zero child exit (process ran but failed)
- `1`: timeout or session error (not found, etc.) — via anyhow bail

Severity order: spawn failure (2) > sidecar lost (3) > dep failed (4) > non-zero exit (42).

DependencyFailed sub-reasons (TimedOut, Killed) are collapsed to code 4 for multi-session aggregation. The sub-reason detail is in the JSON output.

Timeout uses `anyhow::bail!` (exit code 1), consistent with the original single-session wait and other Tender commands. It is an operational error, not a process outcome.

Duplicate session names are deduplicated — `tender wait foo foo` emits one entry.

## Implementation

Poll loop (same 500ms interval as dependency wait). Each iteration checks `meta.json` for every listed session. A session is satisfied when its `RunStatus` is terminal. The loop exits when the completion condition is met (all terminal, or any terminal with `--any`).

## Implementation Tasks

1. Change `wait` CLI to accept multiple positional session arguments
2. Add `--any` flag
3. Update wait loop to track per-session terminal state
4. Emit JSON array output
5. Derive exit code from the set (0 if all exited ok, 42 if any non-zero, 3 if timeout)
6. Update `--host` remote forwarding to pass multiple session args
7. Tests: wait-all, wait-any, wait-timeout, wait-mixed-exit-codes, wait-with-not-found

## Acceptance Criteria

- `tender wait s1 s2 s3` blocks until all three are terminal
- `tender wait --any s1 s2 s3` returns when the first one terminates
- Output is a JSON array of meta snapshots
- Exit code reflects the worst outcome in the set
- `--timeout` applies to the entire wait, not per-session
- Single-session `tender wait foo` still works (array of one)
