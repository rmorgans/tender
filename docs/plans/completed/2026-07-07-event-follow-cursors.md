---
id: event-follow-cursors
depends_on: []
links:
  - ../specs/event-protocol.md
  - 2026-07-07-event-emit-primitive.md
---

# Event Protocol Slice 2 — follow, cursors, re-backed watch

**Shipped 2026-07-07** via PR #7 (main@0b0aa7c). All acceptance criteria
below are test-covered (`cli_events_cursor`, `cli_events_follow`,
`cli_events_logs`, `cli_events_warmstart`, `cli_watch_rebacked`,
`events_read`, and cursor-token round-trip properties in
`proptest_invariants`). Shipped as planned — no schema or semantics
deviations; cursor errors landed as a `#[non_exhaustive]` `CursorError` in
the shared lib core. Slices 3–5 remain unscheduled.

Implements slice 2 of [specs/event-protocol.md](../specs/event-protocol.md)
(the schema owner — envelope/storage/ordering decisions live there, not
here): the live read surface. `tender events` gains `--follow` with cursor
resume (Kubernetes semantics on files, §5.2), and `tender watch` is
internally re-backed by the event log with its output shape frozen (§5.3).
Slice 1 (envelope, append, WAL lifecycle events, emit, replay) shipped
2026-07-07 via PR #4 — see
[2026-07-07-event-emit-primitive.md](2026-07-07-event-emit-primitive.md).

Still daemonless: follow is polling at the shipped 100 ms constant; the
disk is the buffer; no notify hints in this slice (poll remains the
protocol, §5.1).

## Scope (slice 2 only)

1. **`--follow`** — after replay, poll all matched sessions at 100 ms:
   per-segment offset tailing, new-segment pickup (lexicographically later
   names; rotation itself is slice 4 but multi-segment logs already exist),
   and new-session discovery per poll (the shipped `session::list` walk,
   mirroring watch). Ordering contract per §4: events are merge-sorted by
   `(ts, writer, seq)` within each poll batch; across batches arrival order
   is best-effort — no global promise.
2. **Warm-start flags** — `--from-now` (record EOF offsets of sessions
   existing at invocation, skip their history; later-discovered sessions
   replay from their start, matching watch's `--from-now`), `--since
   <rfc3339>` (replay events with `ts >= since`), `--last N` (tail-N by
   merge order across matched sessions — the completer's query). The three
   are mutually exclusive with each other and with `--from-cursor`.
3. **Cursor tokens** (§5.2) — opaque URL-safe base64 over
   `{"v":1,"s":[["<ns>/<session>/events/<seg>.jsonl", offset], …]}`
   (offsets are byte positions after the last fully-consumed line).
   `--from-cursor <c>` resumes exactly: no duplicates, no gaps, because
   segment names are permanent identities (§3.3). Tokens cover event
   segments only — never output.log (see item 5).
4. **`--cursors` bookmarks** — interleave read-time `cursor.bookmark`
   records on stdout every 100 events or 5 s idle:
   `{"kind":"cursor.bookmark","ts":"<rfc3339 µs Z>","cursor":"<token>","derived":true}`.
   Read-time only: no `id`/`writer`/`seq`, never written to segments.
5. **`--include-logs`** — project output.log O/E lines in at read time as
   derived events merged by timestamp:
   `{"kind":"log.stdout"|"log.stderr","ts":"<rfc3339 µs Z>","derived":true,
   "namespace":…,"session":…,"run_id":…,"source":"tender.sidecar",
   "data":{"content":…}}` — no stored identity (§5.1). output.log's f64
   seconds convert to the envelope ts format at projection time. Cursors do
   not cover log offsets: with `--from-cursor --include-logs`, log
   projection starts at the resume wall-clock, documented as best-effort.
6. **Cursor-gone** (§5.2) — a cursor naming a segment file that no longer
   exists ⇒ **exit 44** plus one structured stderr line:
   `{"error":"cursor_gone","gone":["<relpath>", …],"recover":"replay without --from-cursor, or use --since <ts>"}`.
   Defined staleness, defined recovery, never a silent restart from zero.
7. **Re-backed `tender watch`** (§5.3) — when a session has an `events/`
   dir, watch's run-event stream is derived from the event log instead of
   meta-diff polling; sessions without one keep the legacy meta-diff
   synthesis. Output shape **frozen**: f64 `ts`, `kind:"run"`/`name` split,
   the shipped event names, legacy `data` shape (strip the event's
   `provenance` field at projection). Consumers silently gain true
   timestamps, un-collapsed transitions, and real sources (`tender.cli` on
   inferred `run.sidecar_lost`). The logs/annotations paths of watch are
   untouched — they stay output.log-driven.
8. **Shared core** — cursor encode/decode, segment tailing, and the
   log-projection mapping live in the lib (`src/events.rs` or a sibling
   module), consumed by both `commands/events.rs` and `commands/watch.rs`.

## Decisions pinned here (implementation-level; spec stays authoritative)

- `--strict` in follow mode exits 65 on the first observed parse-skip
  (replay-mode semantics extended; a torn line under follow is the same
  defect).
- Poll interval reuses the shipped 100 ms constant — one constant, no new
  configuration surface.
- Bookmark cadence counters (100 events / 5 s idle) reset on each bookmark.
- Cursor token version field `v:1`; unknown version ⇒ treated as
  cursor-gone (exit 44) with `"gone":["<unparseable-token>"]`.

## Non-goals (later slices, per spec §11)

- exec/wrap integration, `TENDER_BLOCK_ID`/`TENDER_PARENT_EVENT_ID`,
  `pty.control_changed`, `callback.finished` (slice 3).
- Rotation, `segment.opened`, `--events-keep-segments`, prune sweep
  additions, `--replace` carry-forward (slice 4).
- `events` in `REMOTE_COMMANDS`, Windows CI, notify wake-up hints
  (slice 5).

## Validation

Canonical example — the Hermes/agent bridge consumer pattern (spec §10):

```sh
tender events --follow --kind hook. --cursors
```

against a session where a Claude Code hook runs
`tender emit --kind hook.post_tool_use --source claude.hook --data-stdin
--best-effort`: the hook event streams out within one poll interval,
bookmarks interleave, and killing + restarting the consumer from its last
bookmark replays nothing twice and drops nothing.

## Acceptance criteria

- `tender events --follow --from-now` on a live session surfaces an event
  emitted by another process within 500 ms, envelope NDJSON, batch-ordered
  by `(ts, writer, seq)`.
- Cursor round-trip exactness: consume N events, stop at a bookmark,
  resume with `--from-cursor` — the remaining events exactly once, across
  a multi-segment log.
- Deleting a cursor's segment then resuming ⇒ exit 44 and the structured
  `cursor_gone` stderr line with a non-empty `gone` list.
- `--last 5` returns exactly the last 5 events by merge order; `--since`
  excludes earlier events; `--from-now` skips history for pre-existing
  sessions but replays later-discovered sessions from their start.
- `--cursors` emits a bookmark within 5 s of idle and after every 100
  events; bookmark records carry `derived:true` and no stored identity.
- `--include-logs` interleaves `log.stdout`/`log.stderr` derived records
  in timestamp order with `derived:true` and no `id`, while the stored
  segments remain untouched (byte-identical before/after).
- A fast-exit session (`echo hi`) watched via re-backed watch shows all
  three lifecycle transitions un-collapsed; legacy watch behavior is
  preserved for a session without an `events/` dir (fixture from a
  pre-slice-1 layout).
- All existing tests stay green; `cli_watch.rs` passes unmodified —
  watch's output shape is byte-identical for the fields it asserts;
  meta.json, output.log, and exec envelopes untouched.
