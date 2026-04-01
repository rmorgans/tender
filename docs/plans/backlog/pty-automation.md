---
id: pty-automation
depends_on:
  - pty-session-mode
links: []
---

# PTY Automation — Agent Control Of Interactive Sessions

Add agent-driven interaction for PTY-backed sessions after PTY session mode exists.

This is the follow-on feature for interactive-program automation. It is intentionally separate from the first PTY slice.

## Goal

Let agents drive interactive terminal sessions safely enough to handle TTY-sensitive programs, while preserving a clear ownership model between humans and agents.

## Why This Is Separate

Writing bytes into a PTY is easy. Recovering structured command boundaries from a PTY transcript is not.

Problems this plan owns:

- prompts and redraws
- merged stdout/stderr
- terminal control sequences
- shell-state ambiguity
- human interference during automation
- command boundary detection in transcript output

That is why PTY automation should not be conflated with first-slice `exec`.

## First Slice Goal

Land a minimal, lease-based PTY control model for agents:

- agent can obtain exclusive control of a PTY session
- agent can send input bytes into the PTY
- agent can observe transcript output
- no human controller may be attached while agent control is active
- results are transcript-based, not stdout/stderr-structured like `exec`

## Model

The PTY broker should eventually support distinct connection modes:

- detached
- observing
- controlling

This plan adds agent control on top of that broker model.

Recommended rule:

- at most one controller at a time
- agent control requires an exclusive lease
- human attach while agent lease is active is rejected or must explicitly steal control

## Relationship To `exec`

`exec` remains the structured, non-PTY command feature.

PTY automation is different:

- transcript-driven
- terminal-oriented
- less structured
- appropriate for interactive or TTY-sensitive programs

Do not try to make PTY automation return the same cleanliness guarantees as non-PTY `exec` in the first slice.

## Candidate CLI / API Surface

This plan deliberately leaves the public interface open until PTY session mode exists.

Possible shapes:

```bash
tender pty-send shell
```

or a more explicit lease/attach-control API.

The key requirement is ownership clarity, not specific flag spelling yet.

## Implementation Tasks

1. Extend PTY session metadata with controller identity / lease state
2. Define agent control lease semantics
3. Add input-write API for PTY sessions
4. Add transcript-follow API suitable for agent consumption
5. Decide how control stealing / preemption works
6. Add tests for controller exclusivity and transcript visibility

## Testing

- agent can acquire control of a detached PTY session
- agent input reaches the terminal process
- transcript output is observable during agent control
- second controller is rejected
- human attach is rejected or explicitly preempts according to policy
- session remains alive if the controlling agent disconnects unexpectedly

## Acceptance Criteria

- interactive programs can be automated through Tender without pretending they are structured non-PTY sessions
- controller ownership is explicit
- human and agent control do not silently interfere with each other

## Depends On

`pty-session-mode` must exist first.

## Not In Scope

- clean stdout/stderr separation
- non-PTY-style `exec` semantics
- multi-controller collaboration
- browser relay
