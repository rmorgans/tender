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

Multiple positional arguments. `--any` returns on the first poll where at least one session is terminal. The output includes all sessions that are terminal at that point (not necessarily just one).

## Output

JSON array of meta snapshots for all waited sessions (consistent with structured-output-only rule):

```json
[
  {"session":"test-unit","status":"Exited","exit_code":0,...},
  {"session":"test-integration","status":"Exited","exit_code":0,...},
  {"session":"lint","status":"Exited","exit_code":1,...}
]
```

With `--any`, the array contains all sessions that are terminal at the point the completion condition is met (may be more than one if multiple sessions terminated between polls).

## Exit Code

Multi-session exit code picks the most severe failure in the set:

- `0`: all sessions exited successfully
- `2`: at least one spawn failure (most severe — process never started)
- `3`: at least one sidecar lost (supervision crashed)
- `4`: at least one dependency failed (upstream exited non-zero)
- `42`: at least one non-zero child exit (process ran but failed)
- `124`: at least one dependency timed out
- `137`: at least one session killed during dependency wait
- `1`: timeout or session error (not found, etc.) — via anyhow bail

Severity order: spawn failure (2) > sidecar lost (3) > dep failed (4/124/137, equal rank) > non-zero exit (42).

DependencyFailed sub-reasons preserve their individual exit codes (4, 124, 137), matching `tender run`. When multiple dep-fail sub-reasons appear in the same set, any one of them wins (they share severity rank).

Timeout uses `anyhow::bail!` (exit code 1), consistent with the original single-session wait and other Tender commands. It is an operational error, not a process outcome.

Duplicate session names are deduplicated — `tender wait foo foo` emits one entry.

## Implementation

Poll loop (same 500ms interval as dependency wait). Each iteration checks `meta.json` for every listed session. A session is satisfied when its `RunStatus` is terminal. The loop exits when the completion condition is met (all terminal, or any terminal with `--any`).

## Implementation Tasks

1. Change `wait` CLI to accept multiple positional session arguments
2. Add `--any` flag
3. Deduplicate session names preserving request order
4. Update wait loop to track per-session terminal state
5. Emit JSON array output (one entry per unique session, in request order)
6. Derive exit code from the set using severity ranking: spawn failed (2) > sidecar lost (3) > dep failed (4/124/137) > non-zero exit (42). Timeout via anyhow::bail (exit 1).
7. Update `--host` remote forwarding to pass multiple session args and `--any`
8. Tests: wait-all, wait-any, wait-timeout, wait-mixed-exit-codes, wait-not-found, wait-duplicates, wait-spawn-failed-beats-nonzero

## Acceptance Criteria

- `tender wait s1 s2 s3` blocks until all three are terminal
- `tender wait --any s1 s2 s3` returns on first poll where at least one is terminal (output includes all terminal sessions at that point)
- Output is a JSON array of meta snapshots in request order
- Duplicate session names produce one output entry
- Exit code reflects the most severe failure in the set (severity-ranked, not numerically smallest)
- Exit codes for DependencyFailed sub-reasons match `tender run`: 4 (failed), 124 (timed out), 137 (killed)
- `--timeout` applies to the entire wait, not per-session; exits code 1 via anyhow::bail
- Single-session `tender wait foo` still works (array of one)
