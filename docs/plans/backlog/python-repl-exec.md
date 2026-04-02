---
id: python-repl-exec
depends_on:
  - explicit-exec-targets
links: []
---

# Python REPL Exec — Side-Channel Results for Interpreter Sessions

Add `PythonRepl` as an exec target with file-based structured results. First exec target that supports PTY transport.

## Why

Python REPLs are a primary agent workspace. Many check `isatty()` for behaviour (readline, colour, progress bars). Agents need structured exec (send code, get exit status, get cwd) without scraping terminal transcript noise.

The current exec model (sentinel in stdout, scraped from log) doesn't work on PTY because of echo, prompts, and ANSI escapes. The fix is a side channel: the injected frame writes structured JSON to a file, Tender polls for that file.

## Design Rule

PTY exec exists only where Tender controls the language runtime enough to produce structured results without scraping terminal noise. Python REPL fits because Tender injects Python code that captures results programmatically. Generic shell PTY does not fit — shell output has no reliable structure.

## Protocol

### Side-channel result files

Each exec invocation writes its result to `{session_dir}/exec-results/{token}.json`.

The injected frame:
1. Runs the user's code inside a `try`/`except`
2. Captures exit code, cwd, exception traceback
3. Optionally captures stdout/stderr via `contextlib.redirect_stdout`/`redirect_stderr`
4. Writes JSON atomically: `{token}.tmp` then rename to `{token}.json`

The exec client:
1. Creates `{session_dir}/exec-results/` if needed
2. Snapshots log cursor (for transcript capture, not result extraction)
3. Pushes the framed Python code into stdin
4. Polls for `{token}.json` (not the transcript)
5. Reads structured JSON, deletes the file

### Injected frame (Python)

```python
exec(compile('''
import json, os, sys, contextlib, io, traceback
_out, _err, _code, _tb = io.StringIO(), io.StringIO(), 0, None
try:
    with contextlib.redirect_stdout(_out), contextlib.redirect_stderr(_err):
        exec(compile(USER_CODE_HERE, '<exec>', 'exec'))
except SystemExit as _e:
    _code = _e.code if _e.code is not None else 0
except:
    _tb = traceback.format_exc()
    _code = 1
_tmp = 'RESULT_PATH.tmp'
with open(_tmp, 'w') as _f:
    json.dump({"exit_code": _code, "cwd": os.getcwd(), "stdout": _out.getvalue(), "stderr": _err.getvalue(), "traceback": _tb}, _f)
os.rename(_tmp, 'RESULT_PATH')
''', '<tender-exec>', 'exec'))
```

Where `USER_CODE_HERE` is the agent's code (triple-quote escaped) and `RESULT_PATH` is the absolute path to `{session_dir}/exec-results/{token}.json`.

### Result JSON

```json
{
  "exit_code": 0,
  "cwd": "/home/user/project",
  "stdout": "hello\n",
  "stderr": "",
  "traceback": null
}
```

### ExecTarget capability profile

```
PythonRepl:
  supports_pipe: true
  supports_pty: true
  result_channel: side_channel
  output_model: captured (via redirect)
```

Compare to existing targets:
```
PosixShell:
  supports_pipe: true
  supports_pty: false
  result_channel: sentinel
  output_model: split (O/E tags in log)

PowerShell:
  supports_pipe: true
  supports_pty: false
  result_channel: sentinel
  output_model: split
```

## CLI

```bash
# Pipe (works today's way, but result via side channel)
tender start py --stdin --exec-target python-repl -- python3
tender exec py -- "print('hello')"

# PTY (new — REPL with isatty() = true)
tender start py-pty --stdin --pty --exec-target python-repl -- python3
tender exec py-pty -- "import numpy; print(numpy.__version__)"

# uv-flavoured python (same target, different launcher)
tender start uv-py --stdin --exec-target python-repl -- uv run python
```

## What Changes in Exec

The exec client (`run_exec` in `exec.rs`) gains a second result path:

- `result_channel == Sentinel` → current log-scanning path (PosixShell, PowerShell)
- `result_channel == SideChannel` → poll for `exec-results/{token}.json`

The PTY rejection check changes from:
```rust
if io_mode == Pty { bail!("exec not supported on PTY") }
```
to:
```rust
if io_mode == Pty && !exec_target.supports_pty() {
    bail!("exec target {} does not support PTY sessions", exec_target)
}
```

## Code Escaping

The user's code must be embedded in the injected frame. Triple-quote escaping:
- Wrap user code in `'''...'''`
- Escape any `'''` in user code as `\'\'\'` or use raw string with sentinel check
- Alternative: base64-encode user code, decode in the frame (`import base64; exec(base64.b64decode('...'))`)

Base64 is simpler and immune to escaping issues. Slight overhead but negligible for REPL use.

## What Does Not Change

- PosixShell and PowerShell stay on sentinel protocol
- Log format is unchanged (JSONL)
- Annotation writing for exec results stays the same
- `tender log`, `tender watch` unaffected

## Implementation Tasks

1. Add `SideChannel` variant to exec result protocol (alongside existing sentinel path)
2. Add `PythonRepl` to `ExecTarget` enum with `supports_pty: true`, `result_channel: SideChannel`
3. Implement `python_frame()` in `exec_frame.rs` — base64-encoded user code, atomic result file write
4. Add side-channel polling path in `run_exec` — poll for `exec-results/{token}.json` instead of log scanning
5. Relax PTY rejection: allow exec when `exec_target.supports_pty()`
6. Add `exec-results/` directory creation in session setup
7. Tests: Python exec on pipe, Python exec on PTY, exit code capture, stdout/stderr capture, cwd capture, traceback capture, timeout, base64 escaping edge cases

## Acceptance Criteria

- `tender exec` on a PythonRepl session returns structured JSON with exit_code, cwd, stdout, stderr
- Works on both pipe and PTY sessions
- PTY transcript shows the REPL interaction (for humans/logging)
- Agent gets clean structured results (no terminal noise)
- Result files are cleaned up after reading
- Base64 encoding handles arbitrary user code without escaping issues

## Not In Scope

- NodeRepl, RubyRepl — future targets, same pattern
- Migrating PosixShell/PowerShell to side-channel — not needed, sentinel works fine
- Generic PTY exec — only where Tender controls the runtime
- Multiline REPL state management (incomplete expressions)
