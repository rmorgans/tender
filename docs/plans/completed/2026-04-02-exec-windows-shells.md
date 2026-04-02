---
id: exec-windows-shells
depends_on:
  - exec
links: []
---

# exec Windows Shell Support

> Compatibility extension for `tender exec` to support PowerShell and cmd.exe sessions.

## Goal

Make `tender exec` work correctly against PowerShell (`pwsh`, `powershell.exe`) and optionally `cmd.exe` sessions on Windows.

Currently `exec` explicitly rejects PowerShell sessions (`src/commands/exec.rs`) and has no cmd.exe detection. The `powershell_frame` builder exists in `src/exec_frame.rs` but uses naive space-joined arguments — not safe for real use.

## Work Items

1. PowerShell-safe argv escaping (handle spaces, quotes, `$`, `;`, backticks)
2. Windows integration tests against `pwsh` / `powershell.exe`
3. Explicit contract for cwd and exit-code sentinel parsing under PowerShell
4. Decision on cmd.exe: support with a `cmd_frame` builder, or reject with clear error
5. Remove the PowerShell rejection guard in `exec.rs` once escaping is correct

## Not In Scope

- PTY-backed shells (separate plan: pty-session-mode)
- Remote execution (separate plan: remote-ssh-transport)
