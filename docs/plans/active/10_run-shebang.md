---
id: run-shebang
depends_on: []
links: []
---

# tender run — Supervised Script Execution

Turn any script into a supervised run. Shebang usage is one entry point, not the only one.

## CLI

```bash
tender run <script> [args...]
tender run --shell python3 <script> [args...]
tender run --detach <script> [args...]
```

## Behavior

### Default (foreground, synchronous)

1. Parse `#tender:` directives from the script header (see Directive Parsing)
2. Derive session name from script filename (see Name Derivation)
3. Build `cmd_start()` arguments from directives + CLI flags
4. Call `cmd_start()` internally (Rust function call, not re-exec)
5. Spawn a log-follow thread that streams `output.log` to the caller's stdout/stderr (same logic as `cmd_log --follow`). This thread runs concurrently with step 6.
6. On the main thread, poll `meta.json` for terminal state (same logic as `cmd_wait()`). Block until the session reaches a terminal state.
7. Once terminal state is reached, drain any remaining log output from the follow thread, then join it.
8. Exit with the child's exit code from `meta.json`.

The concurrency model is: one thread follows the log (blocking reads on `output.log`), the main thread polls for terminal state. The follow thread stops naturally when it sees the terminal state marker in the log, or is joined after the main thread detects termination. No complex synchronization needed — the follow thread is read-only and the main thread only reads `meta.json`.

This makes `tender run build.sh` behave like running `build.sh` directly — output visible, blocks until done, returns the exit code. The session is still supervised (crash recovery, output logging, event stream).

### Detached mode (`--detach`)

Same as steps 1-4 above, then return immediately with session JSON (same as `tender start`). For background jobs where the caller does not want to wait.

## Invocation: Internal, Not Re-exec

`tender run` calls `cmd_start()` as a Rust function. It does NOT shell out to `tender start`. This means:

- The argv stored in `LaunchSpec` is the child command (`["bash", "build.sh"]`), not the tender invocation
- No binary-finding or quoting issues
- Error handling is direct (no subprocess exit code parsing)
- `canonical_hash` is deterministic and matches what `tender start -- bash build.sh` would produce

## Name Derivation

Session name is derived from the script's basename with these steps:

1. Take basename: `/home/user/scripts/build.sh` -> `build.sh`
2. Strip the last extension: `build.sh` -> `build`
3. Replace dots with hyphens: `my.build.sh` -> `my-build`
4. Replace leading underscores: `_private.sh` -> `private`
5. Truncate to 128 chars
6. Validate against `SessionName` rules

If the derived name is still invalid after sanitization, error with a clear message suggesting the `session=NAME` directive.

Examples:

| Script | Derived Name |
|--------|-------------|
| `build.sh` | `build` |
| `my.build.sh` | `my-build` |
| `_private.sh` | `private` |
| `Makefile` | `Makefile` |
| `.hidden.sh` | `hidden` |

## Directive Parsing

Parse `#tender:` lines from the script header. Rules:

- **Start:** first line after the shebang (line 1 if no shebang, line 2 if shebang present)
- **Stop:** at the first line that is not blank and not a `#` comment. Do not scan the entire file.
- **Syntax:** `#tender: key=value` — space after colon required, split on first `=` for the value
- **Whitespace:** leading/trailing whitespace in value is trimmed
- **Unknown directives:** error, not silently ignored. Catches typos like `timout=3600`.
- **Duplicate non-repeatable keys:** error (last-wins would hide mistakes)
- **Comment prefix:** always `#`. This limits polyglot support to `#`-comment languages (bash, python, ruby, perl, R, etc.). Languages using `//` or `--` are not supported via directives — use CLI flags instead.

| Directive | Maps to | Repeatable |
|-----------|---------|------------|
| `namespace=X` | `--namespace X` | No |
| `timeout=N` | `--timeout N` | No |
| `on-exit=CMD` | `--on-exit CMD` | Yes |
| `stdin=pipe` | `--stdin` | No |
| `cwd=/path` | `--cwd /path` | No |
| `env=KEY=VALUE` | `--env KEY=VALUE` (split on first `=`) | Yes |
| `replace` | `--replace` (no value needed) | No |
| `session=NAME` | Override auto-derived session name | No |
| `detach` | `--detach` (no value needed) | No |

CLI flags override directives. Directives override defaults.

## Shell Resolution

- If `--shell` is specified: argv is `[shell, script_path, args...]`
- If not specified and script is executable (`+x`): argv is `[script_path, args...]`
- If not specified and script is not executable: argv is `["bash", script_path, args...]`

This avoids the redundancy of `--shell python3` on a script that already has `#!/usr/bin/env python3`.

On Windows, `+x` is not meaningful. If `--shell` is not specified, always use `["bash", script_path, args...]` as default (or error if bash is not available).

## Shebang Usage

```bash
#!/usr/bin/env -S tender run
#tender: namespace=builds
#tender: timeout=3600
#tender: on-exit=notify-done

make -j8 && cargo test
```

Polyglot (`#`-comment languages only):

```python
#!/usr/bin/env -S tender run --shell python3
#tender: namespace=data
#tender: timeout=7200

import pandas as pd
df = pd.read_csv("big.csv")
```

## Portability

`#!/usr/bin/env -S` support:

| Platform | Support |
|----------|---------|
| GNU coreutils >= 8.30 (2018) | Yes |
| macOS | Yes (all supported versions) |
| FreeBSD >= 6.0 | Yes |
| Alpine/BusyBox >= 1.30 (2019) | Yes |
| Windows | N/A — shebangs are meaningless. Use `tender run <script>` directly. |

## File Validation

`tender run` validates before spawning:

1. Script file exists and is readable
2. Script path resolves to a file (not directory, symlink-to-directory, etc.)
3. Directive parsing succeeds (no unknown keys, no duplicates on non-repeatable keys)
4. Session name derivation succeeds (or `session=` directive provides a valid name)

Fail fast with clear errors before any sidecar is spawned.

## Testing

### Unit tests (directive parsing)

- Parse valid directives from script content
- Error on unknown directive
- Error on duplicate non-repeatable key
- Handle repeatable keys (on-exit, env)
- Stop scanning at first non-comment line
- Handle shebang + directives
- Handle no shebang + directives
- `env=KEY=VALUE` with multiple `=` signs (split on first)

### Unit tests (name derivation)

- `build.sh` -> `build`
- `my.build.sh` -> `my-build`
- `_private.sh` -> `private`
- `.hidden.sh` -> `hidden`
- `Makefile` -> `Makefile`
- Very long filename -> truncated
- Unsanitizable name -> error with suggestion

### Integration tests

- `tender run <script>` blocks until exit, returns child exit code
- `tender run <script>` output visible on stdout/stderr
- `tender run --detach <script>` returns immediately with JSON
- Directives map to correct LaunchSpec fields (check meta.json)
- CLI flags override directives
- `session=NAME` directive overrides auto-derived name
- `--shell python3` uses python3 as interpreter
- Executable script without `--shell` runs directly
- Non-existent script errors cleanly

Test infrastructure: create temp script files in test harness. No new test binaries needed — existing `harness::tender()` + `wait_terminal()` pattern works.

## Scope

`tender run` is sugar over `tender start` + `tender wait` + `tender log --follow`. No new primitives. No new sidecar behavior.

## Not in Scope

- `exec` (sentinel-framed persistent shell) — separate plan
- `--after` dependency chaining — separate plan
- Non-`#` comment directive parsing (use CLI flags for those languages)
