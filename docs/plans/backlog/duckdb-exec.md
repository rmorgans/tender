---
id: duckdb-exec
depends_on:
  - explicit-exec-targets
links: []
---

# DuckDB Exec — Structured SQL for Agent Sessions

Add `DuckDb` as an exec target. Agents send SQL, get structured JSON results back through DuckDB's native output controls.

## Why

DuckDB is a primary tool for data-oriented agents. It queries parquet, CSV, JSON, and Postgres directly with zero setup. Agents today launch `duckdb` in a session and scrape text-table output — lossy, fragile, and wastes tokens parsing ASCII art.

With a DuckDB exec target, agents get structured JSON results from SQL queries through the same exec interface they use for shell commands.

## Design

### ExecTarget variant

```rust
enum ExecTarget {
    None,
    PosixShell,
    PowerShell,
    DuckDb,
}
```

### Protocol composition

| Axis | Choice | Mechanism |
|------|--------|-----------|
| Transport | Pipe | stdin, no PTY needed |
| Escaping | Raw | SQL has no shell metachar issues |
| Exit code | Sentinel presence | `.bail on` stops on error, sentinel never prints |
| CWD | N/A | not a filesystem navigator |
| Result return | SideChannel + Sentinel | `.output` writes JSON to file, `.print` emits sentinel |

### Frame

```sql
.bail on
.mode json
.nullvalue null
.output {session_dir}/exec-results/{token}.jsonl
{user_sql}
.output
.print __TENDER_EXEC__ {token} 0
```

On success: results land in `exec-results/{token}.jsonl` as JSON, sentinel prints with exit code 0.

On error: `.bail on` halts execution, no sentinel emitted, no result file (or partial). Tender detects missing sentinel = failure.

### Result structure

DuckDB `.mode json` outputs a JSON array of row objects per statement. Multiple statements produce concatenated arrays in the `.jsonl` file. Tender does not parse the SQL results — it passes them through to the caller.

The sentinel line carries only exit code (0 or missing). CWD is not tracked — DuckDB sessions don't navigate filesystems.

### Parse changes

`parse_sentinel` needs to handle missing CWD for DuckDB targets. Options:

- Return `cwd: None` (change return type to `Option<PathBuf>`)
- Use a placeholder like `.` (simpler, no type change)

Recommend placeholder `.` — keeps the sentinel format uniform across targets.

### CLI

```bash
tender start db --stdin --exec-target duckdb -- duckdb mydata.duckdb
tender start db --stdin -- duckdb                # inferred from argv[0]
tender exec db -- "SELECT count(*) FROM 'sales.parquet'"
```

### Inference

Add to the argv[0] inference table in `start`:

| argv[0] pattern | Inferred target |
|----------------|-----------------|
| `duckdb` | `DuckDb` |

### Session setup

On first exec (or at session start via an init frame), inject:

```sql
.bail on
```

This ensures error handling is consistent. The per-exec frame sets `.mode json` and `.output` each time so previous exec state doesn't leak.

## Edge Cases

**Multiple statements**: `SELECT 1; SELECT 2;` produces two JSON arrays concatenated in the output file. This is valid JSONL. Agents can parse each array separately.

**DDL statements**: `CREATE TABLE ...` produces no output rows. The result file is empty (or not created). The sentinel still prints — success with no data.

**Dot-commands in user SQL**: If the user sends `.mode csv` as part of their SQL, it overrides the frame's `.mode json`. The next exec resets it. This is acceptable — the user explicitly asked for it.

**Long-running queries**: No special handling. The exec timeout mechanism works the same as for shells — if the sentinel doesn't appear within the timeout, exec reports failure.

## Implementation Tasks

1. Add `DuckDb` variant to `ExecTarget` enum
2. Add `duckdb` to argv[0] inference table in `start`
3. Add `duckdb_frame()` to `exec_frame.rs`
4. Create `exec-results/` directory in session path on first DuckDB exec
5. Handle missing CWD in sentinel parse (use `.` placeholder)
6. Add tests: DuckDB exec on pipe, multi-statement, error handling, inference

## Acceptance Criteria

- `tender start --exec-target duckdb --stdin -- duckdb` stores `exec_target: DuckDb`
- `tender start --stdin -- duckdb` infers `DuckDb`
- `tender exec db -- "SELECT 42 as answer"` returns structured JSON in result file
- Failed SQL (syntax error) reports failure, no sentinel
- Multiple statements produce valid concatenated JSON
- DDL with no output rows succeeds with empty result file

## Not In Scope

- Parsing or transforming SQL results — Tender passes them through
- DuckDB extensions management
- Remote DuckDB (MotherDuck) — would need auth, separate concern
- PTY transport — DuckDB works fine on pipe
