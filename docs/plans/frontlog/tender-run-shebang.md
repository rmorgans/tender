---
id: run-shebang
title: "tender run — Supervised Script Shebang"
created: 2026-03-17
closed:
depends_on: []
links: []
---

# tender run — Supervised Script Shebang

Turn any script into a supervised run by changing the shebang.

## CLI

```bash
tender run <script> [args...]
tender run --shell python3 <script> [args...]
```

## Shebang Usage

```bash
#!/usr/bin/env -S tender run
#tender: namespace=builds
#tender: timeout=3600
#tender: on-exit=notify-done

make -j8 && cargo test
```

Polyglot:

```python
#!/usr/bin/env -S tender run --shell python3
#tender: namespace=data
#tender: timeout=7200

import pandas as pd
df = pd.read_csv("big.csv")
```

## Behavior

1. Parse `#tender:` directives from the script (key=value, one per line)
2. Derive session name from script filename (e.g. `build.sh` → `build`)
3. Build LaunchSpec from directives + CLI flags
4. Call `tender start <session> [--namespace ...] [--timeout ...] [--on-exit ...] -- <shell> <script> [args...]`
5. Default shell is `bash` if `--shell` not specified

`tender run` is sugar over `tender start`. No new primitives.

## Directives

| Directive | Maps to |
|-----------|---------|
| `namespace=X` | `--namespace X` |
| `timeout=N` | `--timeout N` |
| `on-exit=CMD` | `--on-exit CMD` (repeatable) |
| `stdin=pipe` | `--stdin` |
| `cwd=/path` | `--cwd /path` |
| `env=KEY=VALUE` | `--env KEY=VALUE` (repeatable) |
| `replace` | `--replace` |
| `session=NAME` | Override auto-derived session name |

CLI flags override directives. Directives override defaults.

## Portability

`#!/usr/bin/env -S` requires GNU coreutils ≥8.30 (2018) or macOS. Universal on any modern system. Same pattern as `#!/usr/bin/env -S uv run --script` (PEP 723).

## Depends On

All dependencies are satisfied.

## Why Frontlog

Lowest-friction entry point to Tender. No config files, no wrapper scripts, no manual `tender start`. Change the shebang, get supervision. Good for:
- Build scripts
- Data pipelines
- CI jobs
- Anything you'd currently run with `nohup` or `screen`
