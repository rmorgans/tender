# GC / Prune

Clean up old session directories.

## Scope

- `tender prune` — delete sessions older than threshold
- `tender prune --namespace <ns>` — scoped cleanup
- `tender prune --dry-run`
- Configurable retention (e.g. `--older-than 7d`)

## Depends On

- Namespace (frontlog, Phase 2B Slice 1)

## Why Frontlog

Without prune, sessions accumulate forever. Any real cmux trial running multiple workspaces will hit this quickly. Small scope — 1 day of work — but blocks sustained use.
