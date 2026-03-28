# Changelog

## v0.2.0 — Agent Terminal Integration

The minimum credible release for reactive process supervision consumers like terminal UIs and agent orchestrators.

### New features

- **`--cwd` and `--env` on start** — child processes launch in the requested working directory with environment overrides. Inherited environment is preserved; overrides are additive.

- **`--namespace` on all commands** — sessions are grouped by namespace (`~/.tender/sessions/<namespace>/<session>/`). Default namespace is `"default"` when omitted. Two sessions with the same name can coexist in different namespaces.

- **`--on-exit` callbacks** — repeatable flag on `start`. Callbacks execute after terminal state is durable and the session lock is released. Callback results stored in `~/.tender/callbacks/<run_id>.json`, keyed by run_id (survives `--replace`). Six `TENDER_*` environment variables exported to callbacks.

- **`tender watch`** — multiplexed NDJSON event stream. Emits `run` and `log` events using the canonical event envelope. Flags: `--namespace`, `--events`, `--logs`, `--from-now`. Polling-based (100ms). Incremental log tailing. Status dedup. Broken pipe = clean exit.

### Architecture

- **Two state machines:** Run lifecycle ends at terminal meta.json write. Callbacks are a separate post-exit workflow, running after the session lock is released. `--replace` is no longer blocked by slow callbacks.

- **Canonical event envelope:** Frozen schema with fields `ts`, `namespace`, `session`, `run_id`, `source`, `kind`, `name`, `data`. Phase 2B emits `run` and `log` kinds from `tender.sidecar` source.

- **Platform trait extended:** `spawn_child` now accepts `cwd` and `env`. Windows skeleton compiles with the new signature.

### Tests

218 tests (up from 178 in v0.1.0). New coverage for namespace isolation, on-exit callbacks, watch event stream, boundary validation, env inheritance, and idempotency with cwd/env/namespace.

### Known limitations

- Watch is polling-based (100ms). Native filesystem backends (kqueue, inotify, ReadDirectoryChangesW) are a future optimization seam.
- Windows backend is signature-compatible but stub-only. Integration tests fail at spawn_child. 4 pre-existing session_fs test failures on Windows.
- No annotation events or `tender wrap` yet (planned for next release).
- No `tender prune` yet (planned for next release).
- Callback timeout is not enforced — a hung callback keeps the sidecar process alive.

## v0.1.0 — Core Local Supervision

Initial release. 8 CLI commands, Unix process supervision, crash recovery, idempotent start, log capture, stdin push, timeout enforcement.
