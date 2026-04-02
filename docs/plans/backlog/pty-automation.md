---
id: pty-automation
depends_on:
  - pty-session-mode
links: []
---

# PTY Automation — Agent Control of Interactive Sessions

Agent-driven control of PTY-backed sessions with exclusive lease-based ownership, observe-only output streaming, and human preemption.

## Why

Pipe+exec handles structured command execution. But some programs need a real terminal — password prompts, REPLs, TUIs, `ssh`, `psql`. An agent needs to drive these programmatically without a human in the loop, but a human must be able to take over when needed.

The hard problem (command boundary detection in terminal transcript) is explicitly out of scope. This plan provides the plumbing: exclusive control, input, output observation, ownership handoff.

## Current State

PTY session mode is implemented:
- `start --pty` spawns via `openpty`, sidecar owns PTY master
- `push` writes bytes through FIFO to PTY master via `SharedWriter`
- `attach` gives a human full terminal control via Unix domain socket
- `PtyControl` enum: `AgentControl`, `HumanControl`, `Detached`
- Control transitions written to `meta.json`
- Attach protocol: `MSG_DATA`, `MSG_RESIZE`, `MSG_DETACH`
- `capture_stream_with_tee` tees PTY output to attached human

## Design

### Lease Model

An agent acquires exclusive control via a named lease stored in `PtyMeta`:

```json
{
  "pty": {
    "enabled": true,
    "control": "AgentLeased",
    "lease": {
      "agent_id": "claude-session-abc",
      "acquired_at": 1773654000.0,
      "ttl_secs": 300
    }
  }
}
```

- Lease is explicit — `push` alone does not grant a lease
- Lease has a TTL (default 5 minutes) to handle agent crashes
- Agent refreshes lease to keep it alive
- Expired lease is auto-released by sidecar

### Control State Model (extended)

```
start --pty → AgentControl
                 ↓ pty-control acquire
              AgentLeased ←→ HumanControl
                 ↓ pty-control release / expiry
              AgentControl
```

- `AgentControl`: no lease, push allowed, attach allowed
- `AgentLeased`: agent holds lease, push allowed, observe allowed, attach steals (human preemption)
- `HumanControl`: human attached, push rejected, lease suspended (restored on detach)
- `Detached`: no push channel

### Input

`push` remains the input mechanism. No new input command. Push is allowed in `AgentControl` and `AgentLeased`, rejected in `HumanControl`. The lease is about ownership exclusivity and human preemption, not push gating.

### Output Observation

New `tender observe` command — read-only socket connection to the PTY attach socket:
- Sends `MSG_OBSERVE` as first message on connect
- Sidecar tees output to observer without changing control state
- Multiple observers allowed simultaneously
- No input accepted on observe connections
- Agent-friendly: no raw terminal mode, no resize, just streaming bytes to stdout

Alternative: `tender log --follow --raw` already works. `observe` is lower-latency (socket vs file poll) but optional for first slice.

### Human Preemption

Human `attach` always wins:
1. If `AgentLeased`: sidecar transitions to `HumanControl`, suspends lease (kept in `PtyMeta`)
2. Agent detects preemption by polling `tender pty-control status` (control changed to `HumanControl`)
3. Human detaches → sidecar checks for suspended lease → restores `AgentLeased` automatically
4. Agent resumes pushing

### Lease IPC

File-based, same pattern as `kill_request`:
- Agent writes `pty_lease_request` file (JSON: `{action, agent_id, ttl_secs}`)
- Sidecar polls for it (alongside kill_request poll)
- Sidecar validates, updates in-memory state, writes `pty_lease_response`
- Agent reads response, deletes request file

Actions: `acquire`, `release`, `refresh`

## CLI Surface

```bash
# Acquire exclusive lease
tender pty-control acquire <session> --agent-id <id> [--ttl <seconds>] [--namespace NS]

# Release lease
tender pty-control release <session> --agent-id <id> [--namespace NS]

# Check lease status
tender pty-control status <session> [--namespace NS]

# Observe PTY output (read-only, no control change)
tender observe <session> [--namespace NS]
```

## Schema Changes

### PtyControl enum

Add `AgentLeased` variant:

```rust
pub enum PtyControl {
    AgentControl,
    AgentLeased,
    HumanControl,
    Detached,
}
```

### PtyMeta

Add optional lease struct:

```rust
pub struct PtyLease {
    pub agent_id: String,
    pub acquired_at: f64,
    pub ttl_secs: u64,
}
```

### Attach protocol

Add `MSG_OBSERVE = 0x04` — first message sent on observe connection. Sidecar enters observe mode: tees output, rejects input.

## What NOT to Build

- **Command boundary detection** — agent's problem. Read raw transcript.
- **Structured PTY exec** — use pipe+exec for that.
- **Multi-controller collaboration** — one controller at a time.
- **Browser terminal relay** — attach is always local CLI to local sidecar.
- **Windows ConPTY** — Unix only.
- **Automatic prompt injection** — no OSC 633, no shell integration.
- **Lease persistence across sidecar restart** — sidecar dies = session is `sidecar_lost`.

## Implementation Tasks

### Task 1: Extend PtyMeta with lease fields

Add `PtyLease` struct, `PtyControl::AgentLeased` variant, optional `lease` field to `PtyMeta`.

Files: `src/model/pty.rs`

Tests:
- PtyMeta serializes/deserializes with optional lease
- Existing meta without lease still deserializes
- AgentLeased variant serializes correctly

### Task 2: Lease file IPC in sidecar

Add `pty_lease_request` file polling to sidecar (alongside kill_request poll). Sidecar validates request, updates in-memory state, writes `pty_lease_response`, updates `meta.json`.

Files: `src/sidecar.rs`

Tests:
- Sidecar picks up acquire request, writes response, updates meta
- Sidecar rejects acquire when different agent holds lease
- Sidecar processes release request
- Sidecar processes refresh request (resets TTL)

### Task 3: `pty-control` CLI command

Add `pty-control` subcommand with `acquire`, `release`, `status` actions. Writes request file, polls for response with timeout.

Files: `src/main.rs`, `src/commands/pty_control.rs`

Tests:
- acquire + status shows lease
- release clears lease
- second acquire by different agent rejected
- acquire on non-PTY session fails
- acquire on non-running session fails

### Task 4: Human preemption of agent lease

When human attaches while `AgentLeased`: transition to `HumanControl`, suspend lease. On detach: restore `AgentLeased` if suspended lease exists and hasn't expired.

Files: `src/sidecar.rs` (attach listener)

Tests:
- Human attach preempts agent lease, meta shows HumanControl
- Human detach restores AgentLeased
- Expired suspended lease is not restored on detach

### Task 5: Lease expiry

Sidecar checks `acquired_at + ttl_secs` on each poll cycle. Expired lease auto-releases to `AgentControl`.

Files: `src/sidecar.rs`

Tests:
- Lease expires after TTL, status shows AgentControl
- Refresh resets expiry timer
- Expired lease allows new acquire by different agent

### Task 6: Observe-only socket connection

Add `MSG_OBSERVE` message type. Change `attach_sink` from `Option<Box<dyn Write>>` to support multiple observers. `tender observe` command connects, sends MSG_OBSERVE, streams output to stdout.

Files: `src/attach_proto.rs`, `src/sidecar.rs`, `src/main.rs`, `src/commands/observe.rs`

Tests:
- Observe receives output without changing control state
- Push works while observer connected
- Multiple observers work
- Observer disconnect doesn't affect control state

### Task 7: SSH remote forwarding for new commands

Add `pty-control` and `observe` to SSH allowlist and `remote_args()`.

Files: `src/main.rs`, `src/ssh.rs`

Tests:
- pty-control forwarded correctly
- observe forwarded correctly

### Task 8: Integration tests

End-to-end scenarios covering the full lease lifecycle.

Tests:
- Agent acquires lease, pushes commands, releases lease
- Agent acquires, human attaches (preempts), human detaches (restores), agent resumes
- Agent crashes (lease expires), new agent acquires
- Observe during agent control
- Observe during human control

## Implementation Order

Tasks 1 → 2 → 3 form the minimal useful slice (lease model + CLI + IPC).
Task 4 adds human preemption (critical for the use case).
Task 5 adds crash safety (lease expiry).
Task 6 adds observe mode (nice-to-have, `log --follow` is the fallback).
Tasks 7-8 are cross-cutting.

Core value: tasks 1-5. Tasks 6-8 are independently deferrable.
