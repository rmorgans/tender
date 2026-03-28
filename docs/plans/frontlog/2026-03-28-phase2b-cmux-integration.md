# Phase 2B — cmux Integration Minimum Release

**Goal:** Ship the smallest credible Tender release that `cmux` can actually wire in and evaluate as a replacement for its current local process-lifecycle stack.

**Date:** 2026-03-28

**Scope:** Local macOS-first integration for `cmux`. Remote relay replacement is explicitly out of scope for this phase.

---

## Executive Summary

Tender is already good enough to supervise a local child process, persist state, capture logs, recover from sidecar loss, and expose a clean CLI contract.

Tender is **not** yet good enough to be a drop-in backend for `cmux`.

The missing pieces are:

1. **Launch fidelity**
   `LaunchSpec` already includes `cwd` and `env`, but the current Unix spawn path does not apply them.

2. **Namespace grouping**
   `cmux` needs session grouping at the workspace level, not one flat global session namespace.

3. **Push lifecycle callbacks**
   `cmux` is reactive. It cannot depend on polling `status` to learn that Claude exited or supervision failed.

4. **Multiplexed watch stream**
   `cmux` needs one connection per workspace that carries state changes and logs for all supervised sessions in that workspace.

5. **Packaging**
   `cmux` maintainers need an installable release artifact, not a source-only prototype.

The minimum credible release is therefore:

1. Wire `cwd` and `env` through child spawn.
2. Add `--namespace` end-to-end.
3. Add `--on-exit` end-to-end.
4. Add `tender watch`, especially `tender watch --namespace`.
5. Cut a tagged binary release.

---

## cmux Today

### Architecture Snapshot

`cmux` currently spans three distinct process-management layers:

1. **Claude wrapper**
   `Resources/bin/claude`
   Injects `--session-id` and Claude hook settings, then `exec`s the real `claude`.

2. **Local socket control plane**
   `Sources/TerminalController.swift`
   Owns the Unix socket listener, accept loop, per-client handling, authentication policy, and listener recovery.

3. **Remote relay daemon**
   `daemon/remote/cmd/cmuxd-remote/main.go`
   Runs JSON-RPC over stdio for remote stream forwarding and relay state.

Relevant upstream files:

- `/tmp/codex-inspect/cmux/Resources/bin/claude`
- `/tmp/codex-inspect/cmux/Sources/TerminalController.swift`
- `/tmp/codex-inspect/cmux/CLI/cmux.swift`
- `/tmp/codex-inspect/cmux/daemon/remote/cmd/cmuxd-remote/main.go`

### What the Claude Wrapper Does

The wrapper is not just a convenience shim. It is the event bridge between Claude Code and `cmux`.

Current behavior on `main`:

- Detect whether the shell is running inside `cmux` via `CMUX_SURFACE_ID`
- Resolve the real `claude` binary
- Inject Claude Code hooks for:
  - `SessionStart`
  - `Stop`
  - `SessionEnd`
  - `Notification`
  - `UserPromptSubmit`
  - `PreToolUse`
- Export `CMUX_CLAUDE_PID`
- `exec` the real `claude`

The key line is that `PreToolUse` is still configured with `"async": true` on current `main`.

### What the CLI Hook Handler Does

The `cmux` CLI consumes Claude hook stdin and translates it into socket commands and notifications.

Current `main` still uses:

- `FileHandle.standardInput.readDataToEndOfFile()` in `runClaudeHook`
- `FileHandle.standardInput.readDataToEndOfFile()` in `runCodexHook`

That means the current upstream `main` branch still has the exact blocking stdin read pattern implicated by the zombie hook bug.

### What the Local Socket Layer Does

`TerminalController` is carrying substantial supervision-adjacent complexity that Tender could absorb:

- socket bind/listen
- accept loop
- client threading
- peer validation
- failure classification
- accept backoff
- accept loop resume
- listener rearm
- path fallback

This is separate from the wrapper problem, but it matters to the integration pitch: `cmux` is already paying a high implementation cost for local process and event coordination.

---

## GitHub Issue Status

As of 2026-03-28:

- Issue `#2248` exists:
  `claude-hook pre-tool-use processes leak — 945+ zombie processes cause system panic`
- PR `#2253` is open and unmerged.
- PR `#2255` is open and unmerged.
- Local `main` checkout still shows the vulnerable shape:
  async `PreToolUse` in the wrapper and blocking hook stdin reads in the CLI.

Implication:

The specific zombie failure mode appears understood upstream, but the fix is not yet on `main`.
This strengthens the argument that `cmux` is still exposed to lifecycle edge cases and that a dedicated supervisor boundary is valuable.

---

## Mapping To cmux Issues

This section is intentionally strict about what Tender would and would not solve.

### What Tender Does Not Solve Directly

Tender is **not** the direct fix for `cmux` issue `#2248`.

`#2248` is caused by the current `cmux` hook path:

- `PreToolUse` is still configured with `"async": true`
- the hook handler still blocks on `readDataToEndOfFile()`

That combination is the immediate bug. `cmux` still needs its own upstream fix for the hook process lifecycle. Tender cannot fix that simply by existing alongside it.

### What Tender Solves Structurally

Tender addresses the broader lifecycle and observability problems around `cmux`'s Claude sessions.

#### 1. Durable process truth

Problem in `cmux`:

- process truth is split across wrapper state, hook callbacks, socket commands, and app-side memory

Tender contribution:

- durable `meta.json`
- durable `run_id`
- durable terminal states
- explicit exit reasons
- crash reconciliation

Effect:

`cmux` no longer has to infer basic process lifecycle from partial signals.

#### 2. Reactive updates without per-session polling glue

Problem in `cmux`:

- the UX is reactive, but the lifecycle stack is fragmented

Tender contribution:

- `--on-exit`
- `watch --namespace`
- one structured event stream per workspace

Effect:

`cmux` can subscribe once per workspace instead of growing more app-side watcher logic.

#### 3. Workspace-scoped grouping

Problem in `cmux`:

- the natural unit is workspace/surface context, not a single flat global session list

Tender contribution:

- `--namespace`

Effect:

- one workspace maps cleanly to one Tender namespace
- one `watch --namespace` stream maps cleanly to one `cmux` workspace

#### 4. Launch fidelity

Problem in `cmux`:

- a serious lifecycle backend must faithfully reproduce the intended working directory and environment

Tender contribution:

- actual `cwd` and `env` application at child spawn

Effect:

`cmux` can trust Tender as an execution substrate rather than a partial babysitter.

### Concrete Mapping

- `#2248` zombie hook leak:
  direct upstream fix required in `cmux`; Tender does not replace that fix
- "how do we know the Claude process exited?":
  Tender terminal state + `--on-exit`
- "how do we observe many sessions in one workspace?":
  Tender `watch --namespace`
- "how do we group runs by workspace?":
  Tender `--namespace`
- "how do we relaunch faithfully?":
  Tender launch fidelity (`cwd` + `env`)
- "how do we reduce custom wrapper/polling glue?":
  Tender semantic run API

### Recommended Positioning

Do not pitch Tender as:

> the fix for `#2248`

Pitch Tender as:

> the simplification of `cmux`'s process supervision and run-event delivery model after the immediate hook bug is fixed upstream

---

## Tender Today

### What Already Works

Tender already has:

- `start`
- `status`
- `kill`
- `push`
- `log`
- `list`
- `wait`
- `_sidecar`

Plus:

- durable `meta.json`
- durable `output.log`
- child tree kill on Unix via process groups
- crash recovery via sidecar-loss reconciliation
- idempotent `start`
- `--replace`
- timeout enforcement

### What Is Missing

#### 1. Launch fidelity gap

`LaunchSpec` already has:

- `cwd`
- `env`
- `namespace`
- `on_exit`

But current Unix spawn only applies:

- `argv`
- stdin mode
- stdout/stderr capture
- process-group setup

This means Tender's serialized spec is ahead of its real runtime behavior.

This gap must be closed before pitching Tender as an integration backend for any serious terminal workflow.

#### 2. No namespace-aware CLI

There is no `--namespace` flag on current CLI commands.

#### 3. No exit callbacks

`on_exit` exists in the model but is not executed by the sidecar.

#### 4. No event stream

There is no `watch` command and no multiplexed session event stream.

#### 5. No packaged release

There are no tags or release artifacts yet.

---

## Minimum Release For cmux

This is the minimum release that allows a serious `cmux` experiment:

### Required

1. **Launch fidelity**
   Apply `cwd` and `env` during child spawn.

2. **Namespace**
   Add namespace-aware session pathing and query semantics.

3. **On-exit callbacks**
   Allow `cmux` to react immediately to process termination without polling.

4. **Watch stream**
   Provide one NDJSON stream for a workspace namespace.

5. **Release packaging**
   Publish tagged binaries and a documented install path.

### Not Required For First Trial

- replacing `cmuxd-remote`
- Windows completion
- built-in webhook transport
- pruning / GC
- fanout

---

## Detailed Plan

This phase should be implemented as four slices, not three.

The first slice is a prerequisite that the earlier sketch did not include.

### Slice 0 — Launch Fidelity

**Why it is required**

`cmux` cannot trust Tender as an execution backend if Tender cannot faithfully launch a child in the intended working directory and environment.

**User-visible changes**

- `tender start` gains:
  - `--cwd <path>`
  - `--env KEY=VALUE` repeatable

**Implementation**

1. Add CLI flags to `start`.
2. Populate `LaunchSpec.cwd` and `LaunchSpec.env`.
3. Extend platform spawn API so child launch takes the full effective launch config, not just `argv` and `stdin_piped`.
4. Apply:
   - `cmd.current_dir(...)` when `cwd` is present
   - `cmd.envs(...)` for env overrides
5. Preserve inherited environment by default, then overlay overrides.
6. Add tests proving:
   - child sees requested `cwd`
   - child sees overridden env vars
   - spec hash changes when `cwd` or `env` changes

**Trait signature change:** The current `Platform::spawn_child` takes only `argv: &[String]` and `stdin_piped: bool`. This must be extended to accept `cwd` and `env` (or a `&LaunchSpec` reference). This is a cross-platform trait break — the Windows skeleton must also accept the new signature even though it returns `Unsupported`. Consider passing `&LaunchSpec` to avoid a growing parameter list.

**Explicitly deferred:** `LaunchSpec.after` (DependencyBinding) is in the same model-vs-runtime gap category but is not needed for cmux and requires complex run_id resolution. Defer to a later phase.

**Acceptance**

- A test child can print its cwd and env and Tender launches it correctly.
- Idempotent spec matching includes `cwd` and `env`.

---

### Slice 1 — Namespace

**Goal**

Group sessions by workspace so `cmux` can treat a workspace as the unit of observation.

**CLI**

- `tender start <name> --namespace <ns> -- ...`
- `tender status <name> --namespace <ns>`
- `tender kill <name> --namespace <ns>`
- `tender push <name> --namespace <ns>`
- `tender log <name> --namespace <ns>`
- `tender wait <name> --namespace <ns>`
- `tender list [--namespace <ns>]`

**Storage**

Session path becomes:

`~/.tender/sessions/<namespace>/<session>`

Default namespace if omitted:

`default`

Using an explicit default namespace keeps the storage model regular and avoids two incompatible directory layouts.

**Implementation**

1. Introduce a validated namespace type.
2. Add namespace-aware session root/path helpers.
3. Update all commands to resolve a session within a namespace.
4. Update `list` to:
   - return one namespace
   - or all namespaces if no flag is supplied
5. Include `namespace` in structured output.
6. Keep backwards compatibility by treating legacy flat paths as `default` during read/migration if needed.

**Sidecar path inference breakage:** The sidecar currently infers session name from `session_dir.file_name()` and root from `session_dir.parent()`. Adding a namespace directory level means `parent()` returns the namespace dir, not the sessions root. Fix by passing namespace explicitly to the sidecar or by including it in the session dir path contract.

**list() iteration:** `session::list()` currently iterates `root.path()` entries directly (flat). With namespaces, it needs either two-level iteration (list all namespaces, then sessions within each) or namespace-scoped iteration when `--namespace` is provided.

**Acceptance**

- Two sessions with the same name can exist in different namespaces.
- `list --namespace foo` only returns `foo`.
- `watch --namespace foo` later has a clean unit to subscribe to.

---

### Slice 2 — On-Exit Callbacks

**Goal**

Let `cmux` receive immediate exit signals without polling.

**CLI**

- `tender start <name> --on-exit '<command>' -- ...`
- `--on-exit` repeatable

**Behavior**

After the sidecar writes the terminal `meta.json`, it executes each `on_exit` command in order.

This ordering matters:

1. terminal state becomes durable
2. callback runs

That gives callback consumers a stable source of truth to query.

**Callback environment**

Export:

- `TENDER_SESSION`
- `TENDER_NAMESPACE`
- `TENDER_RUN_ID`
- `TENDER_GENERATION`
- `TENDER_EXIT_REASON`
- `TENDER_EXIT_CODE`
- `TENDER_SESSION_DIR`

Optional:

- `TENDER_META_PATH`
- `TENDER_OUTPUT_LOG_PATH`

**Execution model**

- fire each callback as a best-effort child process
- capture stdout/stderr to a separate callback log file
- append callback failures to `warnings` in `meta.json`
- do not mutate final lifecycle state if a callback fails

**Security:** Callbacks must be exec'd as direct argv (split on first space or pass as argv array), not passed through `sh -c`. Shell interpretation of callback strings is a command injection surface.

**Implementation**

1. Parse repeatable `--on-exit`.
2. Persist commands in `LaunchSpec.on_exit`.
3. After terminal state write, execute callbacks.
4. Record warnings on non-zero or spawn failure.
5. Add tests for:
   - callback runs after normal exit
   - callback runs after forced kill
   - callback sees env vars
   - callback failure only adds warnings

**Acceptance**

- `cmux` can point `--on-exit` at a tiny helper that writes to its socket or invokes `cmux notify`.
- Terminal state exists before callback fires.

---

### Slice 3 — Watch

**Goal**

Provide a single long-lived stream per namespace carrying lifecycle and log events.

This is the key feature for `cmux`.

**CLI**

- `tender watch`
- `tender watch --namespace <ns>`
- `tender watch --events`
- `tender watch --logs`
- `tender watch --from-now`

Default:

- events + logs enabled
- all namespaces if no `--namespace` is provided

**Wire format**

NDJSON using the canonical event envelope from the design spec. One object per line.

Phase 2B emits `run` and `log` kinds only, from `tender.sidecar` source. No annotations, no `emit`, no global sequencing.

Examples:

```json
{"ts":1774651234.123456,"namespace":"ws-a","session":"claude-1","run_id":"019...","source":"tender.sidecar","kind":"run","name":"run.started","data":{"status":"Running"}}
{"ts":1774651234.223456,"namespace":"ws-a","session":"claude-1","run_id":"019...","source":"tender.sidecar","kind":"log","name":"log.stdout","data":{"content":"Thinking..."}}
{"ts":1774651235.123456,"namespace":"ws-a","session":"claude-2","run_id":"019...","source":"tender.sidecar","kind":"run","name":"run.exited","data":{"reason":"ExitedOk","exit_code":0}}
```

**Format decisions:**
- Timestamps: epoch seconds with microsecond precision (float). Matches `output.log` native format. No ISO 8601 conversion — consumers can format as they wish.
- Stream tags: `output.log` uses `O`/`E`. Watch translates to `log.stdout`/`log.stderr` in the `name` field.
- Namespace: derived from directory structure (after Slice 1), not from parsing `meta.json`. This avoids `Option<String>` handling for legacy sessions without a namespace field.
- No `seq` field. Ordering is by stream position plus `ts`. Source-local sequencing may be added later if needed.

**Source model**

- `run` events come from `meta.json` state transitions
- `log` events come from `output.log` appends
- `annotation` events are not supported in Phase 2B

**Implementation approach**

macOS-first:

- use `kqueue`/FSEvents polling hybrid or a simple portable polling loop for v0
- correctness matters more than elegance for the first release

A polling implementation is acceptable for v0 if:

- it is bounded and efficient
- it emits deduplicated state transitions
- it follows log appends incrementally

Do not overfit to inotify/kqueue in the first cut if that slows delivery.

**Internal design**

1. Discover sessions in scope.
2. Maintain per-session watchers:
   - last seen meta fingerprint
   - last read log offset
3. Emit initial snapshot unless `--from-now`.
4. Continue until interrupted.

**Acceptance**

- One `tender watch --namespace <workspace>` stream is enough for `cmux` to observe all sessions in that workspace.
- State transitions are emitted exactly once per change.
- Log output is tailed incrementally without replaying the full file.

---

## Release Work

After the four slices:

1. Tag `v0.2.0`
2. Build release binaries for:
   - macOS Apple Silicon
   - macOS Intel
   - Linux x86_64 if easy
3. Publish release notes with:
   - current scope
   - known limitations
   - `cmux` integration example
4. Add installation docs
5. Add a Homebrew formula or tap update

---

## Suggested cmux Trial Flow

Once `v0.2.0` exists, the `cmux` trial should be small and explicit:

1. Keep the `Resources/bin/claude` wrapper temporarily.
2. Replace direct lifecycle ownership with Tender for one workspace/session path.
3. Use:
   - `tender start --namespace <workspace> ...`
   - `tender watch --namespace <workspace>`
   - `--on-exit 'cmux ...'`
4. Leave `cmuxd-remote` untouched.
5. Measure:
   - fewer stray helper processes
   - fewer socket-side lifecycle edge cases
   - simpler event coordination

That is enough to validate the architecture before proposing deeper replacement.

**Precondition:** the upstream `cmux` hook leak fix should land first. The first Tender trial should not be framed as a substitute for merging the immediate `#2248` fix.

---

## Risks

### 1. Watch implementation scope creep

Risk:
Trying to build a perfect cross-platform filesystem event system before shipping.

Mitigation:
Ship a correct macOS-first implementation, even if it uses polling internally.

### 2. Namespace migration complexity

Risk:
Breaking existing sessions or complicating path lookup.

Mitigation:
Use `default` namespace and provide a simple compatibility read path.

### 3. Callback semantics becoming a second orchestration layer

Risk:
`on_exit` grows into a general event bus too early.

Mitigation:
Keep Phase 2B limited to post-terminal callbacks only.

### 4. Pitching Tender before upstream cmux fixes land

Risk:
The discussion gets framed as "solve our current bug for us" rather than "simplify your architecture."

Mitigation:
Acknowledge that `cmux` already has active bug-fix PRs for the immediate zombie issue. Position Tender as the longer-term simplification and supervision boundary.

---

## Recommendation

Implement Phase 2B in this order:

1. Slice 0 — Launch fidelity
2. Slice 1 — Namespace
3. Slice 2 — On-exit callbacks
4. Slice 3 — Watch
5. Package `v0.2.0`

If time is tight, do not skip Slice 0.

Without launch fidelity, the rest is an integration story built on a partially fake launch spec.
