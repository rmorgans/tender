---
id: pty-session-mode
depends_on: []
links: []
---

# PTY Session Mode — Interactive Terminal Sessions

Add a PTY-backed session mode to Tender so terminal-sensitive programs can run under supervision and humans can attach, detach, and later reattach.

This is not just a human escape hatch. It is a second execution lane for interactive programs.

## Why This Exists

Some programs need a real terminal:

- shells used interactively
- password prompts
- REPLs
- full-screen TUIs
- `ssh`, `psql`, `python`, and other TTY-sensitive tools
- later, expect-style agent automation

Those programs do not fit cleanly into the current pipe-based session model.

## First Slice Goal

Land the smallest useful PTY-backed session mode:

- start a session in PTY mode
- attach one controlling client
- detach without killing the session
- reattach later
- record a merged transcript to `output.log`

First-slice contract:

- PTY mode is explicit at session start
- one attached client total
- attach is controlling by default
- detach keeps the session alive
- `push` and `exec` are not supported on PTY sessions in slice one

## CLI Surface

```bash
tender start shell --pty -- /bin/bash
tender attach shell
tender attach shell --namespace ws-1
```

## Session Model

PTY sessions are distinct from the existing non-PTY sessions.

Two execution lanes:

- non-PTY sessions
  - machine-friendly
  - support `push`
  - support future `exec`
  - preserve stdout/stderr separation
- PTY sessions
  - interactive-terminal-friendly
  - support `attach`
  - record merged transcript output
  - do not support `push` or `exec` in slice one

## State Model

Do not overload the run lifecycle state machine.

Lifecycle remains:

- `Starting`
- `Running`
- terminal states

PTY attachment is separate session metadata, for example:

```json
{
  "pty": {
    "enabled": true,
    "attached": false
  }
}
```

First slice only needs attached vs detached. Observe-only vs controlling can come later.

## Architecture

Platform split:

- Unix: PTY master owned by the sidecar (`forkpty` or equivalent)
- Windows: ConPTY pseudoconsole owned by the sidecar

The sidecar becomes the PTY broker:

- owns the master side of the terminal
- mirrors terminal output into `output.log`
- brokers attached client input and output
- handles terminal resize events

## Attach Protocol

First slice should use a local attach endpoint owned by the session:

- Unix: Unix domain socket in the session dir
- Windows: named pipe in the session namespace

Attach flow:

1. CLI resolves the target session
2. CLI connects to the session's attach endpoint
3. sidecar verifies the session is PTY-enabled and not already attached
4. sidecar begins bidirectional relay between the client terminal and the PTY
5. client detach closes the relay without killing the supervised child

## Logging Model

Because PTY merges stdout and stderr, `output.log` becomes transcript-oriented:

- preserve timestamping
- store the merged output stream
- continue to emit `watch` events from the merged transcript
- document that stderr separation is unavailable in PTY mode

This is acceptable because PTY mode is for interactive workflows, not clean machine parsing.

## Implementation Tasks

1. Add `--pty` flag to `start`
2. Extend LaunchSpec and meta to record PTY mode
3. Add platform PTY creation APIs to the sidecar layer
4. Replace stdout/stderr pipe wiring with PTY wiring when PTY mode is enabled
5. Add attach endpoint creation and lifecycle management
6. Add `tender attach` CLI command
7. Implement detach handling and resize propagation
8. Reject unsupported operations like `push` and `exec` on PTY sessions in slice one
9. Add integration tests on Unix and Windows

## Testing

- `start --pty` launches an interactive shell session
- `attach` to a PTY session shows shell prompt and output
- detach leaves the session running
- kill while attached terminates cleanly
- second attach attempt gets a busy error
- resize events propagate to the child
- `attach` against a non-PTY session fails clearly
- Windows ConPTY path satisfies the same high-level contract

## Acceptance Criteria

- a human can take over a live supervised session without killing it
- detach preserves the session
- attach semantics are consistent across Unix and Windows
- PTY mode is clearly separated from the default non-PTY session model
- unsupported combinations (`push`, `exec`) fail clearly on PTY sessions

## Depends On

No technical blockers. PTY session mode is useful locally without `exec` or remote SSH.

## Not In Scope

- observe-only attach mode
- shared multi-viewer attach sessions
- browser terminal relay
- PTY-backed `push`
- PTY-backed `exec`
- agent-driven terminal automation
