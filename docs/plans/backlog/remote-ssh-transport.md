---
id: remote-ssh-transport
depends_on: []
links: []
---

# Remote Backend And SSH Transport

Add remote execution without introducing a second lifecycle model.

## Goal

Keep Tender's core promise unchanged across hosts:

- same run model
- same state machine
- same structured output
- same event stream semantics

Remote must be an access path to Tender, not a fork of Tender's lifecycle logic.

## First Slice Decision

The first implementation should be a thin SSH transport over the existing Tender CLI contract.

That means:

- local execution continues to use the current direct code paths
- remote execution shells out to the system `ssh` client
- the remote side runs the existing `tender` binary
- JSON and NDJSON contracts stay unchanged

Do not introduce a general backend refactor in the same slice unless the code forces it. The first slice should prefer a narrow adapter over a wide local architecture rewrite.

## CLI Surface

First-slice CLI:

```bash
tender --host user@box start job -- cmd
tender --host user@box status job
tender --host user@box list
tender --host user@box log -f job
tender --host user@box push job
tender --host user@box kill job
tender --host user@box wait job
tender --host user@box watch --events
```

Rules:

- `--host` is a global flag
- host is an SSH destination string resolved by the system `ssh` client
- namespace semantics are unchanged
- JSON and NDJSON output is passed through without inventing a second schema

## Architecture

Remote invocation in the first slice may be as simple as:

```bash
ssh host tender ...
```

That is acceptable.

The remote transport is not a second lifecycle model. It is a transport wrapper around the existing Tender command contract.

## First Slice Scope

Commands supported remotely in slice one:

- `start`
- `status`
- `list`
- `log`
- `push`
- `kill`
- `wait`
- `watch`

Expected behavior:

- remote `start` returns the same JSON metadata contract as local
- remote `watch` preserves NDJSON framing
- remote `log -f` streams incrementally rather than buffering whole output
- remote command exit codes are preserved when `tender` is reached successfully

## Error Model

Classify failures into three buckets:

1. SSH transport failure
   - DNS / host resolution failure
   - authentication failure
   - connection timeout
   - transport disconnect mid-stream
2. Remote Tender invocation failure
   - `tender` missing on remote host
   - incompatible remote version
   - malformed JSON / NDJSON from remote process
3. Remote supervised process failure
   - same contract as local: child exit, timeout, killed, sidecar lost

The CLI should preserve remote Tender exit codes when invocation succeeds, and reserve a distinct local error path for SSH transport failure.

## Streaming Commands

Commands with streaming output need explicit first-slice behavior:

- `log -f`
- `watch`
- long-running `wait`

Requirements:

- stream SSH stdout directly to the local stdout
- do not buffer the full stream before parsing
- preserve NDJSON framing for `watch`
- if SSH disconnects mid-stream, return a transport error immediately

## Remote Bootstrap Assumption

First slice assumes `tender` is already installed remotely and discoverable on `PATH`.

If the remote binary is missing, return a clear operator-facing error. Automatic binary copy, version install, or refresh is follow-on work.

## Fanout Boundary

Fanout is not transport.

Fanout is orchestration over many backends.

So:

- `tender fanout` belongs above SSH transport
- fanout should work over any backend that satisfies the semantic command contract
- SSH transport should not hide multi-host orchestration inside itself

## Future Broker / Relay Work

Broker or relay work is explicitly deferred.

If added later, it should be understood as infrastructure beneath the remote transport, for concerns like:

- connection reuse
- persistent streaming sessions
- authentication caching
- multiplexing
- remote binary bootstrap

It must not create a second run model or second event model.

## Implementation Tasks

1. Add a global `--host` CLI flag
2. Define which commands are remote-backed in slice one
3. Implement a small SSH invocation helper around the system `ssh` client
4. Pass remote stdout and stderr through without reformatting human output
5. Preserve JSON and NDJSON parsing expectations for commands like `status`, `list`, and `watch`
6. Map SSH invocation failures to distinct local errors
7. Add integration tests against a local SSH test target or fake `ssh` shim
8. Document remote install assumptions and version expectations

## Testing

- remote `status` returns the same JSON shape as local
- remote `start` launches a session and returns metadata
- remote `list` returns the same JSON array shape as local
- remote `log -f` streams continuously
- remote `watch` preserves NDJSON framing
- remote `push` works against a session started over SSH
- invalid SSH target returns transport error, not child-process error
- missing remote `tender` returns a clear operator-facing error
- Windows remote host works if `ssh host tender.exe ...` is configured appropriately

## Acceptance Criteria

- the user can operate a remote Tender instance with the same CLI and output contracts
- transport errors are distinguishable from remote process failures
- streaming commands stay streaming
- no second lifecycle model is introduced to support remote access

## Depends On

No technical blockers. The local backend is stable and Windows hosts are now valid remote targets.

## Not In Scope

- a custom remote daemon by default
- automatic binary copy or upgrade
- proxy tunneling
- browser relay semantics
- connection pooling or multiplexed SSH sessions
- fanout orchestration
