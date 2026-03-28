# Slice 1 — Namespace Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `--namespace` to all CLI commands so sessions are grouped by workspace. Session paths become `~/.tender/sessions/<namespace>/<session>`. Default namespace is `"default"`.

**Architecture:** New validated `Namespace` type in ids.rs. Session path helpers gain a namespace parameter. All commands thread namespace through. Sidecar infers namespace from directory structure (parent of parent). No backwards compatibility with legacy flat paths in v0 — clean break.

**Tech Stack:** Rust, clap, serde, std::fs.

**Quality gates:** `cargo clippy --all-targets` must pass. `cargo fmt` before each commit.

**Conventions:** All new tests use `SERIAL.lock()` guard and `TempDir`. Cleanup long-lived sessions.

---

## Per-Slice Invariant Table

| Invariant | Why it matters | Enforced by | Tested by |
|-----------|---------------|-------------|-----------|
| Namespace validation matches SessionName rules | Consistent input validation | Namespace::new() reuses same rules | model_ids tests |
| Session path is `root/<namespace>/<session>/` | Namespace isolation | session::create/open path construction | namespace_isolation test |
| Default namespace is "default" when omitted | Consistent storage layout | CLI defaults in main.rs | existing tests still pass (default ns) |
| Two sessions with same name in different namespaces coexist | Workspace isolation | Different directory paths | same_name_different_namespace test |
| list --namespace filters to one namespace | Workspace-scoped queries | session::list with namespace param | list_with_namespace test |
| Sidecar extracts namespace from directory structure | Sidecar must know its namespace for meta | parent().parent() in run_inner | sidecar tests pass with namespace paths |
| Meta JSON output includes namespace | Consumers need namespace in structured output | Meta serialization | start output contains namespace |

---

## Batched Execution

### Batch 1: Namespace type + session path helpers

**Task 1: Add Namespace validated type to ids.rs**

- Add `Namespace` struct to `src/model/ids.rs` after `SessionName`
- Same validation as SessionName (no `/`, `.`, whitespace, leading `_`; max 255 bytes)
- Add `Namespace::default_namespace()` returning `Namespace("default")`
- Add serde Serialize/Deserialize
- Add tests in `tests/model_ids.rs`

**Task 2: Update session path helpers**

- Modify `session::create()` to accept `&Namespace` — path becomes `root/<ns>/<name>/`
- Modify `session::open()` to accept `&Namespace`
- Modify `session::open_raw()` to accept `&Namespace`
- Modify `session::list()` to accept `Option<&Namespace>`:
  - Some(ns): list sessions in `root/<ns>/`
  - None: list all namespaces, then all sessions within each
- Create namespace directory in `create()` before session directory
- Update `SessionDir` to carry namespace

**Task 3: Fix sidecar path inference**

- In `src/sidecar.rs` run_inner(): session_dir is now `root/<ns>/<name>/`
- Extract session name from `session_dir.file_name()`
- Extract namespace from `session_dir.parent().file_name()`
- Extract root from `session_dir.parent().parent()`

### Batch 2: CLI flags + command wiring

**Task 4: Add --namespace flag to all CLI commands**

- Add `#[arg(long)] namespace: Option<String>` to Start, Push, Status, Kill, Log, Wait
- Add `#[arg(long)] namespace: Option<String>` to List
- Update all dispatch calls in main() to pass namespace
- Resolve namespace: `namespace.map(|s| Namespace::new(&s)).transpose()?.unwrap_or_else(Namespace::default_namespace)`

**Task 5: Thread namespace through all cmd_* functions**

- Update signatures of: cmd_start, cmd_status, cmd_kill, cmd_push, cmd_log, cmd_wait, cmd_list
- Pass resolved Namespace to session::create/open/list calls
- Update handle_replace and try_idempotent_start to accept namespace

### Batch 3: Meta output + tests

**Task 6: Include namespace in structured output**

- Add `namespace` field to Meta (or include in JSON output from commands)
- Ensure `tender start` JSON output includes namespace
- Ensure `tender status` JSON output includes namespace

**Task 7: Integration tests**

- Existing tests must pass (they now use default namespace implicitly)
- New tests:
  - `start_in_explicit_namespace` — verify session created under namespace dir
  - `same_name_different_namespace` — two sessions with same name, different ns, both work
  - `list_with_namespace_filters` — list --namespace returns only that namespace
  - `list_without_namespace_returns_all` — list without flag returns all
  - `start_with_namespace_idempotent` — same ns+name+spec = idempotent
  - `start_with_namespace_conflict` — same ns+name, different spec = conflict

**Task 8: Full suite + quality gates**

- cargo test --tests
- cargo clippy --all-targets
- cargo fmt
