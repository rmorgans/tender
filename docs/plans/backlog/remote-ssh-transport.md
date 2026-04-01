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

## Core Model

The right abstraction boundary is **semantic backend**, not raw packet transport.

Conceptually:

- `TenderCore`
  local sidecar, session store, log store, state machine
- `TenderBackend`
  semantic operations over Tender concepts
- backend implementations
  - local
  - SSH
  - later, possibly brokered

The semantic backend surface is:

- `start`
- `status`
- `list`
- `log`
- `push`
- `kill`
- `wait`
- `watch`

The CLI may expose this as `--host`, but `--host` is a user-facing affordance, not the architectural boundary.

## First Remote Backend

The first remote backend should be:

- `SshBackend`

Behavior:

- execute remote `tender` commands over SSH
- parse the same JSON / NDJSON contract
- preserve Tender exit code semantics

This means remote Tender may invoke:

`ssh host tender ...`

That is acceptable for remote execution. It is not the local architecture.

## What This Phase Includes

- `tender --host user@box start ...`
- `tender --host user@box status ...`
- `tender --host user@box log ...`
- `tender --host user@box push ...`
- `tender --host user@box kill ...`
- `tender --host user@box wait ...`
- `tender --host user@box watch ...`
- `host:session` addressing where useful
- host resolution from SSH config
- error classification:
  - SSH failure
  - remote Tender failure
  - supervised process failure

## What This Phase Does Not Include

- a second remote lifecycle model
- a custom remote daemon by default
- browser/session relay semantics
- proxy tunneling
- making fanout part of transport internals

## Fanout

Fanout is **not** transport.

Fanout is orchestration over many backends.

So:

- `tender fanout` belongs above `SshBackend`
- fanout should work over any backend that satisfies the semantic interface

Examples:

- local fanout
- SSH fanout
- mixed backend fanout later if ever needed

## Broker / Relay

Broker or relay work is explicitly deferred.

If added later, a broker/relay is better understood as a lower-level helper for remote backends, not as Tender's primary remote abstraction.

Possible future broker responsibilities:

- connection reuse
- remote binary bootstrap
- persistent streaming sessions
- authentication/session caching
- multiplexing many requests over one long-lived channel

That is infrastructure below the semantic backend boundary.

It should not be allowed to create a second run model or second event model.

## Bootstrap

Remote bootstrap is a separate concern from lifecycle semantics.

Possible future options:

- require `tender` preinstalled remotely
- copy the binary on first use
- version-check and refresh as needed

This can be added to the SSH backend later without changing Tender core semantics.

## Depends On

No technical blockers — the local backend is stable and tested (237/237 Windows, 234/234 macOS). The previous `depends_on: [run-shebang, wrap-annotation-ingestion]` was sequencing preference, not a real dependency.

## Notes

- Windows hosts are now valid remote targets (Windows backend complete as of 2026-04-01).
- This remains the likely path for any future overlap with `cmuxd-remote`, but only for supervised-run semantics. It is not an attempt to replace all remote relay behavior in `cmux`.

> **Needs full design expansion before promotion to active.** Key decisions: whether to introduce a `Backend` trait (refactoring local code) or use pure SSH-shelling (simpler, no local refactor). Add a concrete task list, error classification spec, and integration test plan.
