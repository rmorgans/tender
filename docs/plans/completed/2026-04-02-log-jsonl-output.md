---
id: log-jsonl-output
depends_on: []
links: []
---

# JSONL Log Format + Slim Down `tender log`

Replace the custom `{secs}.{micros:06} {tag} {content}` log format with JSONL. Simultaneously remove `--grep` from `tender log` — searching is standard tooling's job (`rg`, `jq`, `grep`).

## Goal

1. Every line in `output.log` is a self-describing JSON object. The custom line format and its parser are deleted entirely.
2. `tender log` does only what Tender uniquely can: session-aware follow, relative timestamp filtering, content extraction, and remote forwarding. Pattern matching is delegated to standard tools via pipe.

## Why

**JSONL storage:** The custom format requires a custom parser (`LogLine::parse`), cannot represent structured annotation content natively, and forces agents to either parse a bespoke format or lose metadata with `--raw`. JSONL makes every line parseable by `jq`, `rg`, and any JSON library.

**Drop `--grep`:** Tender's `--grep` is a substring match with no regex support. `rg` is SIMD-accelerated, supports full regex, and already exists on every dev machine. Reimplementing search inside Tender is wasted code. The right pattern is:

```bash
tender log --namespace ci --session build | rg "error"
tender log --namespace ci --session build --follow | rg --line-buffered "FAIL"
```

## What `tender log` Keeps

These are domain-specific features that standard tools cannot replicate:

| Flag | Why Tender owns it |
|------|--------------------|
| `--since DURATION` | Relative duration math (`30s`, `5m`) against log timestamps |
| `--tail N` | Knows the session path, avoids user constructing it |
| `--follow` | Waits for file creation, stops when session terminates |
| `--raw` | Strips JSON envelope, emits content only |
| `--host` | Forwards over SSH to remote Tender |

## What `tender log` Drops

| Flag | Replacement |
|------|-------------|
| `--grep PATTERN` | `tender log ... \| rg PATTERN` |

## JSONL Format

Every line in `output.log` is a JSON object followed by a newline:

```json
{"ts":1773653954.012345,"tag":"O","content":"hello world"}
{"ts":1773653954.012346,"tag":"E","content":"warning: unused variable"}
{"ts":1773653954.012347,"tag":"A","content":{"source":"claude.hook","event":"pre-tool-use","data":{}}}
```

Fields:
- `ts`: epoch seconds as float (matches `watch` envelope convention)
- `tag`: `"O"` (stdout), `"E"` (stderr), `"A"` (annotation)
- `content`: string for O/E lines, JSON object for A lines

Tag filtering is standard tooling's job:

```bash
tender log --session build | jq 'select(.tag == "A")'
tender log --session build | rg '"tag":"O"'
```

## What Changes

### Write path (sidecar, wrap, exec)

`capture_stream` and `capture_stream_with_tee` in `sidecar.rs` write JSONL directly. `timestamp_micros()` returns an `f64` instead of a formatted string. The `format!("{ts} {tag} {line}\n")` calls become `serde_json::to_string` calls.

`wrap` annotation writes and `exec` annotation writes use the same JSONL format.

### Read path (log.rs)

`LogLine::parse` is replaced by `serde_json::from_str::<LogLine>`. The `LogLine` struct derives `Serialize, Deserialize`. `format_prefixed` is deleted. `format_raw` returns `content` as a string (stringifying JSON for annotation lines).

`matches_query` drops the grep check. Only `since_us` filtering remains in-process.

### CLI (tender log)

`--grep` flag is removed. `--raw` remains. No `--format` flag. No `--tag` flag. Output is JSONL by default; `--raw` strips the envelope.

### Existing tests

All tests that write or assert on the old format are updated to write/assert JSONL. All `--grep` tests are deleted. No compatibility shim for reading old-format logs.

## Migration

Old-format `output.log` files from previous sessions are not readable by the new parser. This is acceptable:
- `tender prune` cleans old sessions
- Sessions are ephemeral by design
- No migration path needed — old logs are dead

## Implementation Tasks

1. Add `Serialize, Deserialize` to `LogLine`, change `timestamp_us: u64` to `ts: f64`, change `tag: char` to `tag: String`, make `content` an enum or `serde_json::Value` for annotation support
2. Replace `LogLine::parse` with `serde_json::from_str`. Delete `format_prefixed`. Update `format_raw` to stringify annotation content.
3. Update sidecar `capture_stream` and `capture_stream_with_tee` to write JSONL
4. Update `wrap` annotation writes to use JSONL
5. Update `exec` annotation writes to use JSONL
6. Remove `--grep` flag from CLI, remove `grep` field from `LogQuery`, remove grep filtering from `matches_query`
7. Update all tests (log.rs unit tests, cli_log integration tests, cli_wrap tests, cli_exec tests, sidecar tests). Delete grep-specific tests.

## Acceptance Criteria

- `output.log` contains one JSON object per line, no exceptions
- `LogLine::parse` does not exist
- `--grep` flag does not exist
- `jq '.' output.log` works on any session log
- `--raw` emits content-only lines
- `--since`, `--tail`, `--follow` work as before
- Annotation lines carry parsed JSON in `content`, not stringified JSON
- No code references the old `{secs}.{micros:06} {tag}` format
- `tender log ... | rg pattern` works for searching (JSONL streams cleanly through pipes)
