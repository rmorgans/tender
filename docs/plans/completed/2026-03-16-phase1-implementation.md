# Phase 1: Core Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Local Linux/macOS agent process sitter — single binary, structured output, TDD throughout.

**Architecture:** One crate, monolith-first per Rust skill. Boundary trait only at OS seam.

**Tech Stack:** Rust 1.75+, clap (derive), serde/serde_json, anyhow/thiserror, tracing, uuid (v7), tokio (follow/wait)

---

## Module Layout

```
src/
  main.rs                  # clap entrypoint, subcommand wiring
  lib.rs                   # module exports, shared helpers
  model/
    mod.rs                 # re-exports
    ids.rs                 # RunId, Generation, SessionName, ProcessIdentity
    spec.rs                # LaunchSpec, dependency binding, canonical hash
    state.rs               # RunStatus, terminal states, exit taxonomy
    meta.rs                # persisted Meta schema v1
    transition.rs          # legal state transitions only
  session.rs               # session dir create/open, atomic meta.json, lock guard
  sidecar.rs               # _sidecar command, readiness handshake, child supervision
  log.rs                   # parse/query output.log: tail/follow/grep/since/raw
  output.rs                # JSON default, --human renderer  (Note: deferred — JSON-only output shipped in Phase 1)
  platform/
    mod.rs                 # narrow OS boundary: Platform trait
    unix.rs                # fork/setsid, process-group kill, pid birth time, mkfifo
    windows.rs             # Phase 2 — empty stub
```

---

## Core Types

```rust
// ids.rs
pub struct RunId(uuid::Uuid);              // UUID v7, authoritative execution identity
pub struct Generation(u64);                 // monotonic, debug/reuse counter only
pub struct SessionName(String);             // validated: no slashes, no dots, non-empty
pub struct ProcessIdentity {
    pub pid: NonZeroU32,
    pub start_time_ns: u64,
}

// state.rs
pub enum RunStatus {
    Starting,
    Running,
    Terminal(TerminalState),
}

pub enum TerminalState {
    SpawnFailed,
    ExitedOk,
    ExitedError(NonZeroI32),
    Killed,
    KilledForced,
    TimedOut,
    SidecarLost,
}

// spec.rs
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub timeout_s: Option<u64>,
    pub after: Vec<DependencyBinding>,
    pub namespace: Option<String>,
    pub on_exit: Vec<String>,
    pub stdin_mode: StdinMode,
}

pub struct DependencyBinding {
    pub session: SessionName,
    pub run_id: RunId,
}

pub enum StdinMode { Pipe, None }

// meta.rs
pub struct Meta {
    pub schema_version: u32,            // always 1
    pub session: SessionName,
    pub run_id: RunId,
    pub generation: Generation,
    pub launch_spec_hash: String,       // sha256 of canonical LaunchSpec
    pub launch_spec: LaunchSpec,
    pub sidecar: ProcessIdentity,
    pub child: Option<ProcessIdentity>, // None while Starting
    pub status: RunStatus,
    pub exit_code: Option<i32>,         // only for ExitedError
    pub started_at: String,             // ISO 8601
    pub ended_at: Option<String>,       // only for Terminal states
    pub restart_count: u32,
}
```

---

## Invariants

These are compile-time or test-enforced. Not aspirational.

| Invariant | Enforcement |
|-----------|-------------|
| run_id is the only execution identity for lifecycle logic | Generation has no methods for binding or comparison |
| Only sidecar writes lifecycle state | CLI only writes `sidecar_lost` during reconciliation |
| Starting has sidecar identity but no child identity | `child: Option<ProcessIdentity>` is None in Starting |
| Running has both sidecar and child identities | Transition to Running requires child ProcessIdentity |
| Terminal states always have ended_at | Transition function sets ended_at |
| Non-terminal states never have ended_at | Type enforced: ended_at set only in terminal transition |
| ExitedOk never carries exit code | Separate enum variant, no code field |
| ExitedError always carries NonZeroI32 | Type enforced |
| start cannot return success until durable Running or SpawnFailed | Readiness handshake blocks CLI |
| Held lock implies exactly one live sidecar | Lock acquired in sidecar, released on exit |
| PID checks always verify (pid, start_time_ns) | ProcessIdentity is the only way to reference a process |

---

## Platform Boundary

```rust
// platform/mod.rs
pub enum KillMode { Graceful, Forced }

pub trait Platform {
    fn spawn_sidecar(
        tender_bin: &Path,
        session_dir: &Path,
        cmd: &[String],
    ) -> Result<ProcessIdentity>;

    fn spawn_child(
        cmd: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
        stdout: File,
        stderr: File,
    ) -> Result<ChildHandle>;

    fn is_alive(id: &ProcessIdentity) -> bool;

    fn kill_tree(id: &ProcessIdentity, mode: KillMode) -> Result<()>;

    fn child_start_time(pid: NonZeroU32) -> Result<u64>;

    fn create_stdin_pipe(session_dir: &Path, session_name: &str) -> Result<StdinPipe>;
}
```

**Not behind the trait:** logs, state transitions, reconciliation, meta.json, session dirs. Those are portable.

---

## TDD Build Slices

### Slice 1: Model

**Files:** `src/model/*.rs`

**Step 1: Write types**

RunId, Generation, SessionName, ProcessIdentity, RunStatus, TerminalState, LaunchSpec, Meta.
SessionName validates: non-empty, no `/`, no `.`, no whitespace.

**Step 2: Write transition module**

Only legal transitions compile. Starting → Running requires child ProcessIdentity. Running → Terminal sets ended_at. No backward transitions.

**Step 3: Write tests**

```
tests/model_ids.rs        — RunId uniqueness, SessionName validation (good + bad)
tests/model_transitions.rs — legal transitions succeed, illegal transitions fail
```

- Serde round-trip for Meta (serialize → deserialize → equal)
- Canonical hash stability (same LaunchSpec → same hash, different spec → different hash)
- Legal transitions: Starting→Running, Running→ExitedOk, Running→Killed, etc.
- Illegal: Running→Starting, Terminal→Running, Starting→ExitedOk (no child)

**Step 4: Run**

```bash
cargo test --tests model_ids model_transitions
cargo clippy --all-targets -- -D warnings
```

**Step 5: Commit**

```bash
git add src/model/ src/lib.rs tests/model_*.rs Cargo.toml
git commit -m "feat: core model types with transition invariants"
```

---

### Slice 2: Session Directory

**Files:** `src/session.rs`

**Step 1: Write session dir operations**

- `create(base: &Path, name: &SessionName) -> Result<SessionDir>`
- `open(base: &Path, name: &SessionName) -> Result<Option<SessionDir>>`
- `list(base: &Path) -> Result<Vec<SessionName>>`
- `read_meta(dir: &SessionDir) -> Result<Meta>`
- `write_meta_atomic(dir: &SessionDir, meta: &Meta) -> Result<()>` — write to .tmp, rename
- `LockGuard` — flock on lock file, Drop releases

**Step 2: Write tests**

```
tests/session_fs.rs
```

- Create session dir, verify structure (meta.json exists after write)
- Open non-existent returns None
- List returns created sessions
- Lock exclusivity: second lock attempt fails or blocks
- Atomic write: crash between write and rename leaves old meta intact
- Schema v1 read/write round-trip

**Step 3: Run**

```bash
cargo test --test session_fs
```

**Step 4: Commit**

```bash
git add src/session.rs tests/session_fs.rs
git commit -m "feat: session directory with atomic meta writes and lock guard"
```

---

### Slice 3: Sidecar Readiness Handshake

**Files:** `src/sidecar.rs`, `src/platform/unix.rs`, `src/main.rs`

**Step 1: Implement _sidecar subcommand**

- Receives session dir path and readiness fd via args/env
- Acquires session lock
- Writes meta.json with Starting state
- Signals readiness to CLI via pipe write
- (No child spawn yet — just proves the handshake)
- Exits

**Step 2: Implement CLI start path**

- Creates session dir
- Opens readiness pipe (read end)
- Spawns sidecar via Platform::spawn_sidecar (fork + setsid on Unix)
- Blocks reading readiness pipe
- Reads meta.json, prints JSON, exits

**Step 3: Write tests**

```
tests/sidecar_ready.rs
```

- `tender start test-job /bin/true` → exits 0, meta.json has Starting→Running state
- `tender status test-job` → returns JSON with session info
- Start without sidecar signaling → CLI times out with error
- Lock is held while sidecar lives

**Step 4: Run**

```bash
cargo test --test sidecar_ready -- --nocapture
```

**Step 5: Commit**

```bash
git add src/sidecar.rs src/platform/ src/main.rs tests/sidecar_ready.rs
git commit -m "feat: sidecar readiness handshake — start blocks until durable state"
```

---

### Slice 4: Child Supervision

**Files:** `src/sidecar.rs`, `src/log.rs`

**Step 1: Sidecar spawns child**

- After readiness, sidecar forks child
- Captures stdout/stderr via pipes
- Writes each line to output.log with `<epoch_us> <O|E> <line>` format
- waitpid on child, writes terminal state to meta.json

**Step 2: Write tests**

```
tests/sidecar_child.rs
```

- `tender start job echo hello` → output.log contains "hello", meta shows ExitedOk
- `tender start job sh -c 'exit 42'` → meta shows ExitedError(42)
- Interleaved stdout/stderr: both tagged correctly in output.log
- Long-running child: kill updates meta to Killed

**Step 3: Commit**

```bash
git commit -m "feat: child supervision with timestamped log capture and exit classification"
```

---

### Slice 5: Core CLI

**Files:** `src/main.rs`, `src/output.rs`

**Step 1: Implement commands**

- `start` — full path with readiness handshake
- `status` — read meta.json, print JSON
- `list` — enumerate session dirs, print JSON array
- `kill` — send signal via Platform::kill_tree

**Step 2: Write tests**

```
tests/cli_start_status.rs
```
(Note: actual test files evolved into `cli_kill.rs`, `cli_kill_forced.rs`, `cli_timeout.rs`, etc.)

- JSON output shape matches schema
- Exit codes match contract (0, 1, 2, 42)
- Kill dead session → exit 0 (idempotent)
- List empty → `[]`

**Step 3: Commit**

```bash
git commit -m "feat: start/status/list/kill with JSON output and exit code contract"
```

---

### Slice 6: Log CLI

**Files:** `src/log.rs`, `src/main.rs`

- `tender log job` — full log
- `tender log job --tail 50` — last N lines
- `tender log job --follow` — tail -f equivalent (tokio file watch)
- `tender log job --grep "ERROR"` — filter lines
- `tender log job --since 5m` — time-windowed
- `tender log job --raw` — strip timestamp+tag prefixes

```
tests/cli_log.rs
```

**Commit:** `feat: log command with tail/follow/grep/since/raw`

---

### Slice 7: Stdin Push

**Files:** `src/platform/unix.rs`, `src/main.rs`

- mkfifo in session dir
- `tender push job` reads stdin, writes to pipe
- Sidecar opens pipe read end, forwards to child stdin

```
tests/cli_push.rs
```

**Commit:** `feat: push command via named pipe to child stdin`

---

### Slice 8: Wait, Idempotency, Reconciliation

**Files:** `src/main.rs`, `src/session.rs`

- `tender wait job` — poll meta.json until terminal, print final state
- `tender wait job --timeout 30` — with timeout
- Idempotent start: hash LaunchSpec, compare on conflict
- `--replace`: kill existing, increment generation, new run_id
- Reconciliation: status detects released lock + non-terminal → writes SidecarLost

```
tests/cli_wait.rs
tests/cli_replace.rs
tests/cli_reconcile.rs
```

**Commit:** `feat: wait, idempotent start, replace, crash reconciliation`

---

## Test Commands

```bash
cargo test                                        # all tests
cargo test --test sidecar_ready -- --nocapture     # specific slice
cargo fmt --check                                  # format check
cargo clippy --all-targets -- -D warnings          # lint
```

---

## Done Criteria

Phase 1 is complete when:

1. `tender start job cmd` → sidecar supervises child, JSON output, correct exit code
2. `tender status job` → current state as JSON
3. `tender list` → all sessions as JSON array
4. `tender kill job` → tree kill, idempotent
5. `tender log job [--tail|--follow|--grep|--since|--raw]` → queryable log
6. `tender push job` → stdin reaches child
7. `tender wait job` → blocks until terminal
8. Idempotent start with launch-spec matching
9. `--replace` with generation increment
10. Crash reconciliation (sidecar_lost)
11. All integration tests pass on Linux and macOS
12. `cargo clippy -- -D warnings` clean
