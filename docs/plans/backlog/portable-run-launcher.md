---
id: portable-run-launcher
depends_on: []
links: []
---

# Portable Run Launcher — Cross-Platform Script Execution

Make `tender run` work reliably on all platforms by replacing the
implicit Bash fallback with explicit interpreter resolution.

## Why

Today `tender run script.sh` falls back to `bash` when the script is
not executable. On Windows, bash is not guaranteed — it only works if
Git-for-Windows or WSL happens to be installed. The current test suite
papers over this by injecting Git Bash into PATH, so test greens are
stronger than the actual product contract.

Exec targets (DuckDB, Python, PowerShell) are already genuinely
cross-platform. The `run` command should meet the same bar.

## Contract

Strict: no hidden Bash fallback on Windows, no inference from
"looks script-like" beyond the explicit supported kinds. Unknown files
hard-fail with clear `--shell` guidance.

## Interpreter Resolution

### Precedence (highest to lowest)

1. `--shell <interpreter>` — explicit override, always wins
2. Shebang (`#!...`) — read first line, extract interpreter (Unix only;
   Windows ignores shebangs)
3. Extension mapping — deterministic, cross-platform
4. Executable bit / PE header — direct execution
5. Fail with actionable error: "unknown script type, use --shell"

### Extension-to-Launcher Mapping

| Extension | Launcher | Notes |
|-----------|----------|-------|
| `.sh` | `bash` | Requires bash on PATH |
| `.ps1` | `pwsh` | Cross-platform PowerShell |
| `.bat`, `.cmd` | `cmd /c` | Windows only |
| `.py` | `py -3` (Windows) / `python3` (POSIX) | Same as test helper pattern |
| `.rb` | `ruby` | |
| `.js` | `node` | |
| `.ts` | Platform-dependent | Needs decision: tsx, ts-node, deno? |
| none | Direct execution if executable, else fail | |

### Windows Python Choice

Use the `py` launcher (`py -3`) on Windows, `python3` on POSIX. This
follows Python's own documentation and avoids the Microsoft Store alias
trap where `python3` resolves to a failure stub.

### Behaviour for Unknown Files

If the extension is not in the mapping and the file is not executable:

```
error: cannot determine interpreter for 'script.xyz'
  hint: use --shell to specify the interpreter
  example: tender run --shell ruby script.xyz
```

No guessing, no silent fallback.

## Interaction with ExecTarget

`tender run` creates a session with `exec_target: None` — it runs a
script to completion, not a persistent REPL. The interpreter resolution
here is orthogonal to ExecTarget: run picks how to *launch* the script,
ExecTarget controls how to *inject commands* into a running session.

If `--exec-target` is set on a `run` session, the launcher mapping is
still used for the initial script, and exec_target governs subsequent
`tender exec` calls.

## Implementation Tasks

1. Add extension-to-launcher mapping function
2. Add shebang parser (Unix; no-op on Windows)
3. Wire precedence chain into `run` command
4. Replace bash fallback with hard fail for unknown types
5. Add `--shell` flag to `run` (if not already present)
6. Update CLI help text with supported script types
7. Cross-platform tests:
   - `.sh` with bash (skip on Windows without bash)
   - `.ps1` with pwsh (skip if pwsh not available)
   - `.py` with platform-appropriate launcher
   - Unknown extension → clear error
   - `--shell` override
   - Shebang-based resolution (Unix only)
   - Executable file → direct launch

## Testing Strategy

Tests must not depend on Git Bash unless the test is explicitly about
Bash. Each test should either:

- Use a universally available interpreter (Python via the `py`/`python3`
  pattern)
- Skip cleanly with `#[cfg(...)]` or runtime detection when the
  required interpreter is absent
- Test the error path (unknown extension → failure message)

## Not In Scope

- Automatic interpreter installation or version management
- Virtual environment detection
- Remote run (covered by `--host` independently)
- Interactive script execution (that's PTY + attach)

## Acceptance Criteria

- `tender run script.sh` works on POSIX, fails clearly on Windows
  without bash
- `tender run script.ps1` works on any platform with pwsh
- `tender run script.py` uses `py -3` on Windows, `python3` on POSIX
- `tender run unknown.xyz` fails with actionable error
- `tender run --shell ruby unknown.xyz` works
- No test depends on Git Bash being in PATH unless explicitly testing
  bash
- `cargo test --test cli_run` is green on both macOS and Windows
