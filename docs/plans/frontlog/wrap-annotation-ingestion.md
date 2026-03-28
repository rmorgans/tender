# Wrap — Transparent Annotation Ingestion

Tap agent hook events without modifying agents or hook scripts.

`wrap` is the public annotation primitive. There is no public `emit`.

## CLI

```bash
tender wrap --session <s> --namespace <ns> --source <src> -- <command> [args...]
```

## Behavior

1. Tee stdin to the wrapped command (live stream, not buffer-then-forward)
2. Capture a copy of stdin for the annotation payload
3. Let the wrapped command run to completion
4. Capture stdout, stderr, and exit code
5. Emit annotation event to session event log
6. Return stdout/stderr/exit code to caller unchanged

Zero modifications to agent or hook script required.

## Annotation Payload

```json
{
  "hook_stdin": { ... },
  "hook_stdout": { ... },
  "hook_stderr": "...",
  "hook_exit_code": 0,
  "command": ["cmux", "claude-hook", "pre-tool-use"],
  "truncated": false
}
```

- `hook_stdin`/`hook_stdout`: parsed as JSON if valid, raw string otherwise
- `hook_stderr`: captured as string
- `truncated`: true if any field hit size limit (default 64KB per field, TBD)

## Why wrap, not emit

- `wrap` preserves provenance — Tender observed the actual execution
- `emit` would only carry "some process asserted this happened"
- `wrap` constrains annotation production to observed command executions
- `wrap` matches the real integration path (hook commands already exist)

If internal `_emit_annotation` is ever needed (SDK, trusted adapters), it stays below the waterline:
- annotation kind only
- cannot emit run events
- cannot use tender.* sources
- explicit source required

## Depends On

- Phase 2B complete (namespace, watch, event envelope)
- Annotation event kind support in watch (`--annotations` flag)

## exec: wrap applied to supervised shells

`tender exec` is a convenience over `wrap` for the persistent shell pattern:

```bash
# Start a supervised shell
tender start myshell --stdin --namespace ws-1 -- /bin/bash

# Run framed commands inside it
tender exec myshell --namespace ws-1 -- ls -la /tmp
tender exec myshell --namespace ws-1 -- make -j8
tender exec myshell --namespace ws-1 -- cd /foo && cargo test
```

Under the hood, `exec` = `push` (framed with sentinel) + `wrap` (observed with provenance).

1. Frame the command with a sentinel: `<cmd>; echo "TENDER_SENTINEL_<id>_$?"`
2. Push framed command to the shell's stdin FIFO
3. Read output.log until sentinel appears
4. Emit annotation event with command, output, exit code
5. Return framed output + exit code as JSON

The shell stays alive between `exec` calls. CWD and env persist. If the agent dies, the shell survives — another agent can reconnect via `tender exec` and keep working.

Watch stream gets both layers:
- Raw output lines as `log` events (from sidecar)
- Framed command results as `annotation` events (from exec/wrap)

**Design work needed:** sentinel framing protocol. Must be collision-resistant (UUID-based), must handle commands that produce binary output, must handle commands that never return (timeout).

`exec` ships as part of wrap, not a separate feature. If sentinel framing proves complex, it can split into its own plan.

## Notes

Works with any agent that uses the stdin/stdout JSON hook pattern: Claude Code (12+ events), Cursor (7), Cline (4), GitHub Copilot (2+). Codex likely compatible but official hook docs are unverified.

The persistent shell + exec pattern maps directly to what Claude Code does internally (persistent bash via pipes with sentinel delimiting). Tender adds supervision, crash recovery, and the event stream that no agent currently has.
