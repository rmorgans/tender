---
id: exec
depends_on:
  - wrap-annotation-ingestion
links: []
---

# tender exec — Sentinel-Framed Command Execution

> **Status: Design phase.** The sentinel framing protocol needs design before implementation.

Run framed commands inside a persistent supervised shell. Each command produces a structured annotation with stdin, stdout, stderr, exit code, and provenance.

## CLI

```bash
tender exec <session> [--namespace <ns>] -- <command> [args...]
```

## Concept

A supervised shell is started once and kept alive:

```bash
tender start myshell --stdin --namespace ws-1 -- /bin/bash
```

Then `exec` sends framed commands through it:

```bash
tender exec myshell --namespace ws-1 -- ls -la /tmp
tender exec myshell --namespace ws-1 -- make -j8
tender exec myshell --namespace ws-1 -- cd /foo && cargo test
```

The shell stays alive between calls. CWD and env persist across invocations. If the agent dies, another agent can reconnect via `tender exec`.

## Under the Hood

`exec` = `push` (framed with sentinel) + annotation capture (observed with provenance).

1. Generate a unique sentinel: `TENDER_SENTINEL_<uuid>`
2. Frame the command: `<cmd>; echo "TENDER_SENTINEL_<uuid>_$?"`
3. Push framed command to the shell's stdin transport
4. Read `output.log` until sentinel line appears
5. Extract exit code from sentinel line
6. Write annotation event with command, captured output, and exit code
7. Return output + exit code to caller

## Design Questions (must resolve before implementation)

### Sentinel collision

UUID-based sentinels are collision-resistant for normal output, but:
- What if the command itself echoes the sentinel string? (Pathological but possible.)
- Proposal: use a sentinel format that includes non-printable bytes or a sufficiently long random token that false matches are astronomically unlikely.

### Binary output

Commands that produce binary output (e.g. `cat image.png`) will corrupt the sentinel scanning. Options:
- Require text-mode output for exec'd commands (document limitation)
- Use a separate channel (sidecar could expose a "command complete" side-signal)
- Base64-encode the sentinel line to make it scannable in binary streams

### Timeout

What happens when a command never returns?
- `exec --timeout 30` kills the command but keeps the shell alive
- How to kill just the command inside the shell without killing the shell itself? `kill %1` or process group tricks inside bash?
- This interacts with the shell's job control capabilities

### Output parsing

`output.log` lines are tagged with `O`/`E`/`A` prefixes by the sidecar. `exec` must:
- Parse tagged lines to extract raw content
- Distinguish stdout vs stderr in the captured output
- Handle interleaved output from concurrent commands (if allowed — probably disallow)

### Error handling

- `push` fails (stdin transport broken): error, shell may be dead
- Shell already exited: error, suggest `tender start` to restart
- Two concurrent `exec` calls on the same shell: undefined behavior? Serialize? Error?
  - Proposal: advisory lock or single-caller enforcement. Concurrent exec is a footgun.

### CWD tracking

"CWD and env persist" is a feature for agents, but there is no mechanism to report the current CWD back to the caller. Options:
- Include `pwd` in the sentinel protocol: `<cmd>; echo "TENDER_SENTINEL_<uuid>_$?_$(pwd)"`
- Accept that CWD is opaque (agents track it themselves)

## Annotation Payload

Same envelope as `wrap`, but with the framing metadata:

```json
{
  "source": "agent.hooks",
  "event": "exec",
  "run_id": "...",
  "data": {
    "hook_stdin": "ls -la /tmp",
    "hook_stdout": "...",
    "hook_stderr": "...",
    "hook_exit_code": 0,
    "command": ["ls", "-la", "/tmp"],
    "sentinel": "TENDER_SENTINEL_<uuid>",
    "truncated": false
  }
}
```

## Watch Integration

Watch stream gets both layers:
- Raw output lines as `log` events (from sidecar, continuous)
- Framed command results as `annotation` events (from exec, per-command)

## Depends On

`wrap-annotation-ingestion` (complete) — `exec` uses the same annotation payload format and the `output.log` infrastructure.

## Not in Scope

- Persistent shell lifecycle management (use `tender start` / `tender kill`)
- Multi-shell orchestration (use separate sessions)
- PTY attachment (separate plan: `pty-attach`)
