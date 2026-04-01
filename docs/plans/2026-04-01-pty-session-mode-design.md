# PTY Session Mode — First Slice Design

## Goal

Add PTY-backed sessions so agents can drive terminal-sensitive programs
(shells, REPLs, interactive prompts) without respawning, and humans can
take over live sessions when needed.

## What this slice delivers

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
- `status`, `list`, `kill`, `wait` — work identically (status adds PTY metadata)
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
"the FIFO push channel exists." For pipe sessions, it means what it
means today.

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
`status` and `watch` can observe control state without any new mechanism.

Non-PTY sessions omit the `pty` field entirely (or `pty.enabled: false`).

### Log format

PTY sessions write merged output with tag `O`. No `E` lines — stderr is
not separable through a PTY. Annotations (`A` tag) work normally.

## Rejected Operations

- `exec` on a PTY session → `"exec is not supported on PTY sessions"`
- `push` in `HumanControl` → `"session is under human control"`
- `attach` on a non-PTY session → `"session is not PTY-enabled"`
- `attach` when already attached → `"session is already under human control"`

## Platform Scope

- Unix: first slice (PTY via `openpty`/`forkpty`, Unix domain socket)
- Windows: deferred (ConPTY, named pipe)

## Not In This Slice

- PTY-backed `exec` (structured command execution over PTY)
- Observe-only attach mode
- Shared multi-viewer sessions
- Browser terminal relay
- Agent-driven terminal automation (expect-style)
- Windows ConPTY support
