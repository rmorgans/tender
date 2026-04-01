---
id: pty-session-mode
depends_on: []
links: []
---

# PTY Session Mode — Interactive Terminal Sessions

> **Slice one scope:** Unix only. Windows ConPTY support is deferred.

Add a PTY-backed session mode to Tender so agents can drive
terminal-sensitive programs without respawning, and humans can take over
live sessions when needed.

## Why This Exists

Some programs need a real terminal:

- shells used interactively
- password prompts
- REPLs
- full-screen TUIs
- `ssh`, `psql`, `python`, and other TTY-sensitive tools
- later, expect-style agent automation

Those programs do not fit cleanly into the current pipe-based session model.

The primary use case is agent-driven: an agent starts a PTY session,
drives it programmatically with `push` (send commands, answer prompts),
and occasionally a human takes over via `attach` without killing the
process.

## First Slice Goal

Land the smallest useful PTY-backed session mode:

- `start --pty` launches a session with a real terminal
- `push` sends raw bytes to the PTY (agent types `y\n`, sends ctrl-c, drives prompts)
- `log` / `watch` observe a merged transcript (no stdout/stderr split)
- `attach` gives a human full terminal control
- detach returns control to the agent
- `exec` is explicitly rejected on PTY sessions
- remote attach works via `--host` with SSH TTY allocation

## Control State Model

```
start --pty → AgentControl ←→ HumanControl
                   ↓
                Detached
```

Rules:

- `start --pty` → `AgentControl`
- `push` allowed in `AgentControl`
- `push` rejected in `HumanControl`
- `attach` steals control → `HumanControl` (single human controller)
- human detach → `AgentControl` if agent lease is still valid, else `Detached`
- agent lease loss → `Detached`

## CLI Surface

```bash
tender start shell --pty -- /bin/bash
tender push shell              # send raw bytes (agent driving)
tender attach shell             # human takes over terminal
tender attach shell --namespace ws-1
tender --host box attach shell  # remote attach via SSH
```

## Session Model

Two execution lanes:

- **pipe sessions** (existing, default)
  - machine-friendly
  - support `push`, `exec`
  - preserve stdout/stderr separation
  - `io_mode: "pipe"`

- **PTY sessions** (new)
  - interactive-terminal-friendly
  - support `push` (raw bytes to PTY)
  - support `attach` (human terminal control)
  - merged transcript output (no stdout/stderr split)
  - `exec` rejected
  - `io_mode: "pty"`

## Architecture

The sidecar is the PTY broker. It already owns the child process
lifecycle — PTY mode extends that to owning the terminal master.

### Sidecar responsibilities in PTY mode

- Allocates a PTY pair (master/slave) instead of pipes
- Spawns the child on the slave side
- Reads from the PTY and writes to `output.log` (merged transcript, tag `O`)
- Listens on a Unix domain socket at `{session_dir}/attach.sock`
- Relays between the attached client and the PTY
- Handles terminal resize (SIGWINCH → `ioctl TIOCSWINSZ` on the PTY)
- Enforces the control state model

### What changes vs non-PTY sessions

| Concern | Non-PTY (current) | PTY (new) |
|---------|-------------------|-----------|
| Child I/O | Separate stdout/stderr pipes | Merged PTY |
| `push` transport | FIFO write | FIFO write (sidecar forwards to PTY) |
| Log format | `O`/`E`/`A` tags | `O`/`A` only (no stderr split) |
| Log semantics | Structured stdout/stderr | Merged terminal transcript |
| `attach` | N/A | Unix socket relay |
| `exec` | Framed stdin + log scrape | Rejected |

### What does NOT change

- The run lifecycle state machine (Starting → Running → terminal states)
- `list`, `kill`, `wait` — work identically
- `status` — works identically but includes PTY metadata
- `watch` events — same NDJSON contract
- Session directory layout — adds `attach.sock`, everything else stays

## Attach Protocol

The attach endpoint is a Unix domain socket at `{session_dir}/attach.sock`,
owned by the sidecar.

### Connection flow

1. `tender attach <session>` opens the socket
2. Sidecar checks: is the session PTY-enabled and Running?
3. Sidecar checks control state — if `AgentControl`, steal to `HumanControl`;
   if already `HumanControl`, reject ("session is already under human control")
4. Sidecar begins bidirectional relay: client terminal ↔ PTY
5. Client puts local terminal in raw mode
6. Client sends current terminal size on connect, and on every SIGWINCH

### Wire protocol on the socket

Minimal framing. Three message types:

| Type | Direction | Payload |
|------|-----------|---------|
| `Data` | Both | Raw terminal bytes |
| `Resize` | Client → Sidecar | `{rows, cols}` |
| `Detach` | Client → Sidecar | Empty (clean disconnect) |

`Data` is the hot path — raw bytes with minimal framing overhead.

### Detach

- Client sends `Detach` message, or just closes the socket (unclean detach)
- Sidecar transitions control state: → `AgentControl` if agent lease valid,
  else → `Detached`
- Child process is unaffected
- Client restores terminal to cooked mode before exiting

### Remote attach

Attach is always local CLI to local sidecar. Remote attach is:

```
tender --host box attach shell
→ ssh -t box tender attach shell
→ remote tender attach opens remote attach.sock
```

SSH carries the terminal. The attach protocol itself is always
sidecar-local. The `--host` dispatch for `attach` uses `ssh -t`
(TTY allocation) instead of the default `ssh -T`.

## Push on PTY Sessions

`push` writes raw bytes to the session's input channel. The contract is
the same as non-PTY sessions — the sidecar routes internally:

- Non-PTY: FIFO → child stdin pipe
- PTY: FIFO → PTY input

The FIFO transport stays the same. The sidecar reads from it and
forwards to the PTY. Push is decoupled from the attach protocol.

Rules:

- `push` in `AgentControl` → accepted, bytes forwarded to PTY
- `push` in `HumanControl` → rejected ("session is under human control")
- Remote push via `--host` works unchanged

## Schema Changes

### LaunchSpec

Add `io_mode` field:

```json
{
  "argv": ["bash"],
  "io_mode": "pty",
  "stdin_mode": "Pipe"
}
```

`io_mode` is `"pipe"` (default, current behavior) or `"pty"`.

`stdin_mode` refers to the push transport availability, not the child's
literal stdin wiring. For PTY sessions, `stdin_mode: "Pipe"` means
"the FIFO push channel exists."

### Meta

Add PTY control state:

```json
{
  "schema_version": 1,
  "session": "shell",
  "status": "Running",
  "pty": {
    "enabled": true,
    "control": "AgentControl"
  }
}
```

`control` is one of: `"AgentControl"`, `"HumanControl"`, `"Detached"`.

The sidecar writes control state transitions atomically to meta.json.
Non-PTY sessions omit the `pty` field entirely.

### Log format

PTY sessions write merged output with tag `O`. No `E` lines — stderr is
not separable through a PTY. Annotations (`A` tag) work normally.

## Rejected Operations

- `exec` on a PTY session → `"exec is not supported on PTY sessions"`
- `push` in `HumanControl` → `"session is under human control"`
- `attach` on a non-PTY session → `"session is not PTY-enabled"`
- `attach` when already attached → `"session is already under human control"`

## Implementation Tasks

1. Add `io_mode` field to LaunchSpec and `--pty` flag to `start`
2. Add PTY control state to Meta
3. Add platform PTY creation API (Unix: `openpty`/`forkpty`)
4. Wire sidecar to use PTY when `io_mode: "pty"` — master/slave instead of pipes
5. Implement merged transcript logging from PTY output
6. Forward FIFO push input to PTY master in sidecar
7. Add attach endpoint (Unix domain socket) creation and lifecycle in sidecar
8. Add `tender attach` CLI command with raw terminal mode and resize
9. Implement control state transitions (AgentControl/HumanControl/Detached)
10. Reject `exec` on PTY sessions, `push` in HumanControl, `attach` on non-PTY
11. Update `--host` dispatch: `attach` uses `ssh -t` instead of `ssh -T`
12. Add integration tests

## Testing

- `start --pty` launches a PTY session, meta shows `pty.enabled: true`
- `push` to a PTY session delivers bytes to the child
- `attach` to a PTY session shows terminal output and accepts input
- detach leaves the session running
- second attach attempt gets a "already under human control" error
- `attach` against a non-PTY session fails clearly
- `exec` against a PTY session fails clearly
- `push` while human is attached gets rejected
- kill while attached terminates cleanly
- resize events propagate to the child
- `log` on PTY session shows merged transcript
- `status` shows PTY control state
- remote attach via `--host` works with TTY allocation

## Acceptance Criteria

- an agent can start a PTY session and drive it with `push`
- a human can take over a live supervised session via `attach`
- detach preserves the session and returns control to the agent
- PTY mode is clearly separated from the default pipe session model
- unsupported operations (`exec`, `push` during human control) fail clearly
- `push` and `attach` work remotely via `--host`

## Platform Scope

- Unix: first slice (`openpty`/`forkpty`, Unix domain socket)
- Windows: deferred (ConPTY, named pipe)

## Depends On

No technical blockers. PTY session mode is useful locally without `exec`
or remote SSH, though remote `attach` via `--host` is supported from
the start.

## Not In Scope

- PTY-backed `exec` (structured command execution over PTY)
- Observe-only attach mode
- Shared multi-viewer sessions
- Browser terminal relay
- Agent-driven terminal automation (expect-style)
- Windows ConPTY support
