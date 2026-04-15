# Tender Architecture

This directory maps the current Tender system as implemented on `main`.

It is architecture-level documentation: process boundaries, session storage, lifecycle state, PTY control, key flows, and transport boundaries. It intentionally does not duplicate every struct field or every helper function.

Read these in order:

1. [01-system-context.md](01-system-context.md) — what the running system is, who talks to it, and where responsibility sits
2. [02-session-storage.md](02-session-storage.md) — what Tender persists on disk, what is durable, and what is transient
3. [03-run-lifecycle.md](03-run-lifecycle.md) — the run state machine and who is allowed to write it
4. [04-pty-lane.md](04-pty-lane.md) — the PTY execution lane and current human/agent control model
5. [05-key-flows.md](05-key-flows.md) — the load-bearing sequences: `start`, `exec`, `kill`, and `attach`
6. [06-transport-boundaries.md](06-transport-boundaries.md) — the concrete IPC and transport surfaces: pipe, file, socket, lock, and SSH

Scope notes:

- These diagrams describe the current codebase, not the full roadmap.
- The planned PTY lease/ownership extension is tracked separately in [../plans/backlog/pty-automation.md](../plans/backlog/pty-automation.md).
- Remote execution is transport-only: the same local lifecycle model is invoked over SSH for the currently allowlisted commands.
