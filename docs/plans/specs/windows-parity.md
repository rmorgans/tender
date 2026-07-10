# Windows Parity — Roadmap

Full Windows parity = a Windows box, as **host or client**, presents the same
**observable contracts** as POSIX for every capability, with regressions blocked
by required native CI. Parity is about contracts, **not** identical OS mechanisms
— Tender does not translate Bash↔PowerShell syntax or Unix↔Windows paths.

Observable-contract parity means: same commands + state transitions; same
stdout/stderr/exit-code and JSON/NDJSON shapes; same persistence across
disconnects; same child-tree ownership + termination guarantees; same pipe/PTY
capabilities; same remote behavior **independent of login shell**; regressions
gated by CI.

## Status (2026-07-10, after v0.2.1)

At parity: **distribution** (native x64+ARM64 binaries, checksums, attestations),
**remote `exec`** (constant-argv frame, shell-neutral). Strong-but-not-absolute:
**local supervision** (Job Objects, ctrl-break — but see lifecycle gaps below),
**PowerShell exec** (works, but ordinary `$x` doesn't persist). Not yet:
**general `--host` control, PTY/attach, lifecycle edge cases, CI**.

## Gap inventory

| Track | Current | Parity gap |
|---|---|---|
| General `--host` | only `exec` framed | other ops traverse POSIX shell quoting |
| PR CI | release builds only (`release.yml` on tag) | no regression gate on any platform |
| Windows child containment | Job Objects work | child briefly runs **before** Job assignment (`windows.rs:145`) |
| SSH durability | breakaway works when permitted | forbidden-breakaway **silently** accepts reduced lifetime (`windows.rs:480`) — a "successful" remote start can die on SSH disconnect |
| Orphan recovery | root PID killable | tree-kill/graceful weaker without the live Job handle |
| PTY | Unix impl | no ConPTY child backend |
| Attach target | Unix socket | no Windows named-pipe carrier |
| Attach client | Unix raw mode | no Windows console frontend |
| Remote attach | `ssh -t` + remote frontend | needs `ssh -T` framed bridge without regressing Windows-client→Unix |
| PowerShell state | `$global:x` persists | ordinary `$x = …` runs in the child scriptblock scope |
| Windows event concurrency | atomic-append contract | no native multi-process stress gate |
| Remote tests | shell-script fake ssh, whole file `cfg(unix)` | can't exercise client behavior on Windows |

## Dependency plan

```
Required PR CI (Phase 0)
    ├── Typed RemoteOperation IR (P1) → framed endpoint (P2) → migrate ops (P3)
    │       unary+stream → push body → delete reconstructed argv
    └── Raw suspended Windows launcher (P4)
            → ConPTY backend → named-pipe carrier → console frontend → ssh -T bridge (P5)
PowerShell scope polish (P6) + final parity matrix → declare parity
```

Remote framing (P1–3) and Windows PTY (P4–5) proceed independently **once CI
exists**. ConPTY follows the launcher hardening — both need raw `CreateProcessW`
+ `STARTUPINFOEXW`.

## Phase 0 — Install the gates first (immediate next PR)

`.github/workflows/ci.yml`, trigger on every PR + push to main; all jobs `--locked`:
- **ubuntu-latest**: fmt, clippy, full suite, `cargo package --locked`
- **macos-latest**: full suite
- **windows-latest**: full native x64 suite
- **windows-11-arm**: full native ARM64 suite
- Then make these **required** via branch protection.

Initial Windows CI will surface cfg-specific warnings + missing test tooling —
**fix those, don't weaken the matrix.**

**Make remote tests portable.** `tests/cli_remote.rs` is entirely `cfg(unix)`
because fake ssh is shell scripts. Replace with a **Rust test-helper exe** that
records argv, captures/forwards stdin, emits configured stdout/stderr + exit
code, and installs as `ssh`/`ssh.exe` on the temp PATH. Then most remote tests
run on all four platforms; keep only genuine POSIX-reconstruction tests
Unix-gated (delete them when the legacy path is removed, P3C).

**Release gating (eventual refactor).** Build/test to temp Actions artifacts →
all lanes must pass → *then* create/publish the release + upload all assets
together → crates.io after. Removes the visible-partial-release window.

## Phases 1–3 — General `--host` via the typed frame

The transport work. Full design in
[remote-frame-transport](../active/00_remote-frame-transport.md); summary:

- **P1** typed `RemoteOperation` IR (Clap `TryFrom` + JSON deserialize → one
  dispatch; **don't** serialize the Clap enum). `StartRequest` carries the FULL
  surface: session, namespace, argv, cwd, env, stdin, pty, timeout, replace,
  `exec_target`, `on_exit`, after, `any_exit`, **boundary + all boundary parents**.
  No SSH change. Acceptance: local byte-compatible; every field survives
  `Commands→StartRequest→JSON→decode→dispatch`; non-UTF-8 remote values fail
  clearly; local-only commands can't enter `RemoteOperation`.
- **P2** framed codec + `ssh -T host tender _remote --frame-from-stdin`: `u32`
  BE header length, UTF-8 JSON header, optional body; 1 MiB header cap; exact
  partial reads; unknown fields tolerated, unknown version/op rejected; full
  semantic validation before side effects; **no user value in SSH argv**;
  actionable stale-remote error; stdout/stderr/exit stay direct. Keep
  `exec --frame-from-stdin` for compat + scripting.
- **P3A** move `start/status/list/log/kill/wait/watch/exec` (no body; `log
  --follow`/`watch` stream after the header). Hostile-content tests on **every**
  string field (argv, cwd, env, callbacks, boundary labels, `log --since`,
  session/namespace; CR/LF, Unicode, quotes, metacharacters, Windows paths).
- **P3B** `push` = header then raw work bytes; test NUL, multi-MiB bounded
  memory, partial writes, early exit, backpressure, exact EOF.
- **P3C** delete reconstructed remote argv; every op emits an identical constant
  SSH argv; POSIX quoting survives only in labelled human fallback text;
  introduce the stricter new-session name grammar (narrow local legacy-name
  cleanup preserved); test real loopback Windows OpenSSH under cmd.exe **and**
  PowerShell. **→ general `--host` reaches Windows parity.**

## Phase 4 — Harden Windows lifecycle guarantees

Before ConPTY, replace `std::process::Command` child launch with a raw launcher:
`CreateProcessW(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP)` → create/configure
Job → assign the suspended process to the Job → resume the primary thread → own
process/thread/pipe handles safely. Closes the escape race where a fast child
spawns descendants before Job assignment.

**SSH breakaway contract.** Silent inheritance of a parent Job when breakaway is
forbidden means a "successful" remote start can die on SSH disconnect. Parity
requires **either** a guaranteed-independent launch via a supported OS mechanism
**or** failing `start` loudly when durable lifetime can't be guaranteed. Silent
degradation must not survive the parity milestone.

Native tests: SSH disconnect doesn't kill a started session; sidecar crash kills
the whole child tree; timeout + forced kill terminate descendants; cooperative
`CTRL_BREAK` exits gracefully; PID reuse never kills the wrong process; repeated
named-pipe connects work; concurrent event writers produce valid complete JSONL.

## Phase 5 — Windows PTY and attach

- **5A ConPTY target**: `CreatePseudoConsole`, I/O pipes,
  `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, `ResizePseudoConsole`,
  `ClosePseudoConsole`, Job assignment before resume, merged-transcript semantics
  identical to Unix. Tests: interactive pwsh/cmd, Unicode, resize,
  detach/reconnect, timeout+tree-kill, PTY Python exec exception, push-rejection
  while human control is active.
- **5B carrier abstraction**: small duplex trait — Unix domain socket / Windows
  named pipe. Keep the existing `MSG_DATA`/`MSG_RESIZE`/`MSG_DETACH` protocol
  carrier-agnostic.
- **5C Windows client frontend**: console mode save/restore (RAII), VT input/output
  flags, raw keyboard, resize events, Ctrl-C + abnormal-exit restoration. Needed
  even for Windows-client→Unix-host.
- **5D remote attach bridge**: move to `ssh -T` (local frontend ↕ attach messages
  ↕ SSH bytes ↕ remote bridge ↕ socket/pipe). Keep `ssh -t` during migration for
  older remotes; remove once capability/version handling prevents regressions.
  **→ PTY/attach reaches Windows parity.**

## Phase 6 — PowerShell behavioral parity

The frame runs user code via `& ([scriptblock]::Create($_code))` → the surprising
child scope. Rework so ordinary assignments persist while Tender's internal
capture variables are cleaned afterward. Acceptance (PS 5.1 **and** 7): `$x = 42`
then later `$x` → 42; `function f {7}` then `f` → 7; `Set-Location`/`$env:` persist.
Preserve: clean stdout/stderr, `$LASTEXITCODE`, terminating + non-terminating
errors, cwd reporting, native exit codes, `exit` terminates the session. Polish
today, but a full-parity exit criterion (POSIX assignments persist naturally).

## Final parity qualification

Do not declare full parity until this matrix is green (✓ = native integration
test or explicit platform-independence proof), for every user-facing command:

| Client | Host | Pipe/exec | General control | PTY/attach |
|---|---|---|---|---|
| POSIX | Windows x64 | ✓ | ✓ | ✓ |
| POSIX | Windows ARM64 | ✓ | ✓ | ✓ |
| Windows x64 | POSIX | ✓ | ✓ | ✓ |
| Windows ARM64 | POSIX | ✓ | ✓ | ✓ |
| Windows | Windows | ✓ | ✓ | ✓ |
| POSIX | POSIX | ✓ | ✓ | ✓ |

## Recommended order

1. Required PR CI (Ubuntu, macOS, Win x64, Win ARM64) — **Phase 0, next**
2. Replace the Unix-only fake-SSH harness
3. Typed DTOs + shared dispatch (Phase 1)
4. Remote frame in operation-sized slices (Phases 2–3)
5. In parallel after CI: raw suspended Windows launcher (Phase 4)
6. ConPTY + attach on that launcher (Phase 5)
7. Ordinary PowerShell state persistence (Phase 6)
8. Final cross-client/host qualification → update the public support claim

Everything after Phase 0 is regression-gated from its first commit.
