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

## Notes

Works with any agent that uses the stdin/stdout JSON hook pattern: Claude Code (12 events), Codex (5), Cursor (7), Cline (4).
