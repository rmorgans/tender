---
id: after-composition
depends_on: []
links: []
---

# --after Dependency Chaining

> **Spec Phase 3, item 21.** Core composition primitive — sidecar polls dependency's meta.json before spawning child.

## CLI

```bash
tender start job2 --after job1 -- cmd              # wait for job1 exit 0
tender start job2 --after job1 --any-exit -- cmd   # wait regardless of exit code
tender start job2 --after job1 --after job3 -- cmd # wait for both
```

## Behavior

1. `--after <session>` adds a dependency to the LaunchSpec
2. At bind time (when `tender start` runs), the CLI captures the target session's current `run_id` from its `meta.json`
3. The dependency is stored as `{ session: "<name>", run_id: "<uuid>" }` in the LaunchSpec
4. The sidecar, before spawning the child, polls each dependency's `meta.json` until:
   - Target reaches a terminal state with exit code 0 -> proceed
   - Target reaches a terminal state with non-zero exit -> fail with exit code 4 (dependency failed)
   - Target's `run_id` changes (was replaced) -> fail with exit code 4 (dependency was replaced, execution identity lost)
   - `--any-exit` flag: proceed on any terminal state regardless of exit code
5. Once all dependencies are satisfied, sidecar spawns the child normally

## LaunchSpec Changes

```json
{
  "argv": ["cmd"],
  "after": [
    {"session": "job1", "namespace": "default", "run_id": "019..."},
    {"session": "job3", "namespace": "default", "run_id": "019..."}
  ],
  "after_any_exit": false
}
```

The `after` array is included in `canonical_hash`. Same command with different dependencies is a different spec (conflict on idempotent start).

## Sidecar Wait Loop

The sidecar polls dependency meta.json files. Design:

- Poll interval: 500ms (same ballpark as existing sidecar poll loops)
- The sidecar holds the session lock during the wait. This means:
  - `tender status` shows `starting` (sidecar is alive but child not yet spawned)
  - `tender kill` can kill the sidecar during the wait (graceful: sidecar writes terminal state)
  - `--timeout` applies to the total time including the wait (not just child runtime)
- If the dependency session does not exist at sidecar start: fail immediately with exit code 4

## Exit Code Semantics

| Condition | Exit Code | State |
|-----------|-----------|-------|
| All deps satisfied, child runs and exits 0 | 0 | exited_ok |
| All deps satisfied, child runs and exits non-zero | 42 | exited_error |
| Dependency exited non-zero (without --any-exit) | 4 | dependency_failed |
| Dependency was replaced (run_id mismatch) | 4 | dependency_failed |
| Dependency session does not exist | 4 | dependency_failed |
| Timeout expired during dependency wait | 3 | timed_out |

Exit code 4 (`dependency_failed`) is already reserved in the spec but not yet implemented.

## State Machine Addition

New terminal state: `dependency_failed`. Transition: `starting` -> `dependency_failed`.

This needs:
- New variant in the state enum
- Serde support
- State transition in the sidecar
- `tender status` display

## Namespace Resolution

`--after job1` resolves the session in the same namespace as the new session (from `--namespace` flag or default). Cross-namespace dependencies are not supported in this plan — if needed later, use `--after ns/job1` syntax.

## Implementation Tasks

1. Add `after` and `after_any_exit` fields to `LaunchSpec`
2. Include `after` in `canonical_hash`
3. Add `--after` and `--any-exit` CLI flags to `Start` command
4. At bind time: resolve each `--after` session, capture its run_id
5. Add `dependency_failed` state to the state machine
6. Implement sidecar dependency wait loop (poll meta.json)
7. Handle run_id mismatch detection
8. Handle timeout interaction (timeout covers wait + child)
9. Integration tests

## Testing

- `start job2 --after job1`: job2 waits for job1, then runs
- `start job2 --after job1` where job1 exits non-zero: job2 fails with exit 4
- `start job2 --after job1 --any-exit` where job1 exits non-zero: job2 runs
- `start job2 --after job1`, then replace job1: job2 fails with exit 4 (run_id mismatch)
- `start job2 --after nonexistent`: job2 fails immediately with exit 4
- `start job2 --after job1 --timeout 5` where job1 never exits: job2 times out
- `kill job2` while waiting on dependency: job2 exits cleanly
- Idempotent start with same --after deps: returns existing session
- Idempotent start with different --after deps: conflict error

## Depends On

Nothing — all prerequisites (sidecar, meta.json polling, state machine, timeout) already exist.

## Not in Scope

- Cross-namespace dependencies
- DAG visualization
- Automatic retry on dependency failure
- Fanout / parallel dispatch (separate plan)
