---
id: exec
depends_on:
  - wrap-annotation-ingestion
links: []
---

# tender exec — Structured Commands In A Persistent Shell

Run commands inside an already-running shell session and get structured results back: stdout, stderr, exit code, and final cwd.

`exec` is the missing layer between:

- `start --stdin`, which gives you a persistent shell
- `push`, which only writes raw bytes into that shell

## First Slice Goal

Land a safe, serialized `exec` for non-PTY shell sessions that were started with `--stdin`.

First-slice contract:

- exactly one `exec` runs against a session at a time
- the target session must already exist and be `running`
- the command is framed and sent through the existing stdin transport
- completion is detected from `output.log` via a unique sentinel line
- the caller gets stdout, stderr, exit code, and final cwd
- an annotation event is written with the structured result

This slice is for agent workflows, not arbitrary terminal emulation.

## CLI

```bash
tender exec <session> [--namespace <ns>] -- <command> [args...]
tender exec <session> [--namespace <ns>] [--timeout 30] -- <command> [args...]
```

Examples:

```bash
tender start shell --stdin -- /bin/bash
tender exec shell -- pwd
tender exec shell -- cd repo
tender exec shell -- cargo test
```

The shell stays alive between calls, so cwd and exported env persist.

## Required Session Properties

The target session must satisfy all of these:

- session exists
- session status is `Running`
- stdin transport is enabled (`--stdin`)
- session is a line-oriented shell session
- session is not PTY-backed

If these are not true, `exec` fails before pushing anything.

## Protocol

`exec` is implemented as:

1. capture a log cursor from the session's `output.log`
2. serialize the requested argv into one shell command string
3. wrap it with a unique sentinel trailer
4. send the framed command through the existing stdin transport
5. tail `output.log` from the captured cursor until the sentinel appears
6. parse stdout/stderr from the tagged log lines
7. extract exit code and final cwd from the sentinel line
8. emit an annotation event
9. return the structured result to the caller

## First-Cut Sentinel Format

The sentinel line must carry:

- random token
- exit code
- final cwd

Unix shell framing:

```sh
<user command>; status=$?; cwd_now="$(pwd)"; printf '__TENDER_EXEC__ %s %s %s\n' "$token" "$status" "$cwd_now"
```

PowerShell framing:

```powershell
<user command>; $status=$LASTEXITCODE; $cwd=(Get-Location).Path; Write-Output "__TENDER_EXEC__ $token $status $cwd"
```

This is enough for the first slice because:

- it preserves shell state
- it makes final cwd observable
- it uses only text output, which matches current `output.log` handling

## Concurrency Model

`exec` is single-flight per session in the first slice.

Implementation rule:

- create an advisory lock such as `exec.lock` in the session dir
- if another `exec` is active, fail immediately with a busy error
- do not attempt to queue or interleave commands

This avoids one caller consuming another caller's sentinel and keeps log parsing tractable.

## Output Model

`exec` reads from the current end of `output.log`, not from the beginning of the session.

Returned result:

- `stdout`: concatenated `O` lines after the log cursor
- `stderr`: concatenated `E` lines after the log cursor
- `exit_code`
- `cwd_after`
- `truncated`

The sentinel line itself is not included in stdout/stderr.

## Timeout Model

First-slice timeout is client-side only.

Meaning:

- `tender exec --timeout 30` stops waiting after 30 seconds
- the shell session remains alive
- the in-shell command may still be running
- the user gets an explicit timeout error that says execution may still be in progress

This is a deliberate first-slice tradeoff. Killing only the currently running in-shell command without killing the shell is follow-on work.

## Binary Output

First slice is text-oriented.

That means:

- binary-heavy commands are unsupported
- sentinel scanning assumes text output
- if binary-safe command execution becomes important later, it should be a protocol revision, not complexity added to slice one

## Annotation Payload

`exec` should write the same top-level envelope shape used by `wrap`, with `event: "exec"` and structured command metadata:

```json
{
  "source": "agent.hooks",
  "event": "exec",
  "run_id": "...",
  "data": {
    "hook_stdin": "cargo test",
    "hook_stdout": "...",
    "hook_stderr": "...",
    "hook_exit_code": 0,
    "command": ["cargo", "test"],
    "cwd_after": "/work/repo",
    "sentinel": "TENDER_EXEC_<uuid>",
    "timed_out": false,
    "truncated": false
  }
}
```

## Watch Integration

Watch should continue to expose both layers:

- raw output lines as `log` events from the sidecar
- structured `exec` results as `annotation` events

`exec` should not invent a second event transport.

## Implementation Tasks

1. Add `Exec` command to the CLI with `--namespace` and `--timeout`
2. Add session preflight checks: running, stdin enabled, non-PTY shell session
3. Add per-session `exec.lock` handling
4. Capture a log cursor before writing to stdin
5. Implement framing builders for Unix shell and PowerShell
6. Reuse the existing stdin transport from `push`
7. Tail `output.log` until sentinel match
8. Parse tagged log lines into stdout/stderr buckets
9. Emit annotation event with the structured result
10. Return command exit code and text output to the caller
11. Add timeout handling with explicit "command may still be running" semantics
12. Add integration tests on Unix and Windows

## Testing

- `exec` against a running bash shell returns stdout and exit code 0
- `exec` propagates non-zero exit code without killing the shell session
- shell state persists across calls (`cd`, exported env)
- `exec` fails if the target session lacks `--stdin`
- `exec` fails if the target session is not running
- second concurrent `exec` on the same session fails with busy error
- timeout returns error while the session remains running
- annotation event contains stdout, stderr, exit code, command, and final cwd
- Windows PowerShell path satisfies the same high-level contract

## Acceptance Criteria

- agents can use `exec` instead of blind `push` for structured shell commands
- cwd persistence is observable and testable
- the feature reuses the existing session, log, and annotation model
- no second caller can corrupt the active `exec` result for a session
- the first slice works on both Unix and Windows

## Depends On

`wrap-annotation-ingestion` is the only real dependency. `exec` reuses the annotation envelope and the existing `output.log` infrastructure.

## Not In Scope

- starting a shell automatically if the session is missing
- killing only the current in-shell command while preserving the shell
- binary-safe framing
- concurrent queued `exec` calls
- PTY-backed shells
- remote execution over SSH
