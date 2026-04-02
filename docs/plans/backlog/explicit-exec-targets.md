---
id: explicit-exec-targets
depends_on: []
links: []
---

# Explicit Exec Targets

Replace argv[0] sniffing with a persisted exec protocol selection in `LaunchSpec`. The session declares what command language it speaks at start time, not at exec time.

## Why

The current exec implementation sniffs `argv[0]` at exec time to decide whether to inject POSIX or PowerShell framing. This is fragile:
- `fish`, `nu`, `elvish` get POSIX framing and break silently
- The agent knows what it launched but Tender guesses anyway
- The classification is repeated on every exec instead of stored once

The fix is making exec target a first-class part of the session identity.

## Design

### ExecTarget enum

```rust
enum ExecTarget {
    PosixShell,
    PowerShell,
}
```

No `Cmd` — cmd.exe is unsupported. No `PythonRepl` — that's a separate protocol (follow-on slice). The enum is non-exhaustive for future extension.

### LaunchSpec change

Add `exec_target: Option<ExecTarget>` to `LaunchSpec`.

- `Some(PosixShell)` — exec uses `unix_frame`
- `Some(PowerShell)` — exec uses `powershell_frame`
- `None` — exec is not supported on this session

`exec_target` is included in spec hash. Different exec target = different session identity.

Deserialization of old sessions without `exec_target` defaults to `None`.

### CLI

```bash
tender start shell --stdin --exec-target posix-shell -- bash
tender start ps --stdin --exec-target powershell -- pwsh
tender start build -- cargo build     # no --exec-target, exec_target: None
```

### Inference

When `--exec-target` is not specified, infer from `argv[0]`:

| argv[0] pattern | Inferred target |
|----------------|-----------------|
| `bash`, `sh`, `zsh` | `PosixShell` |
| `pwsh`, `powershell`, `powershell.exe` | `PowerShell` |
| everything else | `None` |

Inference happens once at `tender start` and is stored. No re-detection at exec time.

### Exec changes

`exec` reads `meta.launch_spec().exec_target` and branches:

- `Some(PosixShell)` → `unix_frame`
- `Some(PowerShell)` → `powershell_frame`
- `None` → `"session has no exec target — restart with --exec-target if this is a shell"`

The `ShellKind` enum and `shell_kind_from_argv0` are deleted.

The PTY rejection check (`io_mode == Pty`) stays as a defensive invariant guard alongside `exec_target`.

## Implementation Tasks

1. Add `ExecTarget` enum to `src/model/spec.rs` with `Serialize`/`Deserialize`, default `None` for backward compat
2. Add `--exec-target` flag to `tender start` CLI, wire to `LaunchSpec`
3. Add inference from `argv[0]` in `start` when `--exec-target` not specified
4. Change `exec` to branch on stored `exec_target` instead of `shell_kind_from_argv0`
5. Delete `ShellKind`, `shell_kind`, `shell_kind_from_argv0` from `exec.rs`
6. Update tests: explicit `--exec-target` in exec tests, test inference, test `None` rejection
7. Update `tender run` if it touches shell classification

## Acceptance Criteria

- `tender start --exec-target posix-shell --stdin -- bash` stores `exec_target: PosixShell` in meta
- `tender start --exec-target powershell --stdin -- pwsh` stores `exec_target: PowerShell` in meta
- `tender start -- cargo build` stores `exec_target: None`
- `tender exec` on a session with `exec_target: None` fails with a clear message
- `tender start --stdin -- bash` infers `PosixShell` without `--exec-target`
- Old sessions without `exec_target` deserialize with `None` (fail closed)
- `exec_target` is included in spec hash
- `ShellKind` and argv[0] sniffing are deleted from exec.rs

## Not In Scope

- `PythonRepl` — separate protocol, separate slice
- `Cmd` — cmd.exe remains unsupported
- `NodeRepl` — future
- PTY exec — separate concern (pty-automation)
