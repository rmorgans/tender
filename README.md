# Tender

Tender is an agent process sitter: a cross-platform CLI for supervised runs.

It is designed to be the execution substrate under agent workflows, hook wrappers, and future frontends. Tender owns run lifecycle, durable session state, logs, annotations, composition, and remote execution. It is not a terminal UI, not a multiplexer, and not a replacement for tools that render panes, tabs, or desktop notifications.

## What Tender Is For

- starting long-lived supervised sessions
- following logs and waiting on terminal state
- pushing stdin into pipe-backed sessions
- running structured commands inside persistent shell sessions with `exec`
- wrapping hooks and writing durable annotation events
- chaining runs with `--after`
- exposing the same semantic model locally and, later, over SSH

## What Tender Is Not

- a GUI terminal
- a tmux replacement
- a PTY-first workflow tool
- a second lifecycle model for remote hosts

The core idea is simple:

```text
Tender = stateless CLI + durable session record + per-session supervisor + OS-native kill/wait
```

The sidecar is the supervisor. The CLI is a transactional client. `meta.json` and `output.log` are the durable truth for a run.

## Current Surface

Core commands today include:

- `start`
- `run`
- `exec`
- `push`
- `log`
- `status`
- `wait`
- `watch`
- `kill`
- `wrap`

These operate on named sessions and stable run identities rather than on raw process trees.

## Design Direction

Tender has one lifecycle model and multiple access paths.

- local execution calls Tender core directly
- remote execution should be the same semantic contract over SSH, not a separate system
- PTY support is secondary and should stay a distinct execution lane from structured non-PTY `exec`

That makes Tender a good foundation for:

- agent supervisors
- hook-driven tooling
- remote orchestration
- higher-level frontends that need durable execution, logs, and control without owning process lifecycle themselves

## Docs

- Design spec: [docs/plans/specs/tender-agent-process-sitter.md](docs/plans/specs/tender-agent-process-sitter.md)
- Planning index: [docs/plans/README.md](docs/plans/README.md)
