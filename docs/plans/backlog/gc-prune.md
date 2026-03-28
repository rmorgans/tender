# GC / Prune

Clean up old session directories.

## Scope

- `tender prune` — delete sessions older than threshold
- `tender prune --namespace <ns>` — scoped cleanup
- `tender prune --dry-run`
- Configurable retention (e.g. `--older-than 7d`)

## Depends On

- Namespace (frontlog, Phase 2B Slice 1)

## Notes

Small feature but important for long-running hosts. Not in original phased plan but implied.
