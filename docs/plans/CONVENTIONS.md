# Docs Conventions

## Work tracking

Work items live in `docs/plans/frontlog/` and `docs/plans/backlog/`. The filesystem is the index.

```
docs/plans/
├── frontlog/          # active queue — semantic slug filenames
│   ├── tender-run-shebang.md
│   └── wrap-annotation-ingestion.md
├── backlog/           # deferred — semantic slug or date-slugged
│   ├── windows-full-backend.md
│   └── pty-attach.md
├── completed/         # archived completed items
```

### Naming

| Location | Naming | Example |
|----------|--------|---------|
| `frontlog/` | Semantic slug or date-prefixed slug | `tender-run-shebang.md` or `2026-03-28-wrap.md` |
| `backlog/` | Semantic slug or date-prefixed slug | `windows-full-backend.md` or `2026-03-14-name.md` |
| `completed/` | Date-prefixed slug | `2026-03-29-windows-child-lifecycle.md` |

Ordering is tracked in `README.md`, not in filenames. Filenames are stable slugs.

### Frontmatter schema

Every work item has YAML frontmatter:

```yaml
---
id: run-shebang                   # semantic, never changes on reorder/archive
title: Short Title
created: 2026-03-17
closed:                           # date when moved to completed/
depends_on: []                    # stable ids, e.g. [run-shebang]
links: []                         # relative paths to related docs
---
```

- **No `status:` field.** Directory placement is the status.
- **Semantic ids** — not queue numbers. `run-shebang` not `frontlog-01`.
- **`depends_on`** references stable ids only — not prose like "Phase 2B complete".
- **One file per item.** Card + implementation plan in the same file.

### Lifecycle

1. New work → create in `frontlog/` with semantic slug filename
2. Deferred work → create in `backlog/`
3. Completed → `git mv` to `completed/`, set `closed:` date in frontmatter

### Prose dependency sections

If a plan body has a "Depends On" section, it must reference stable IDs from frontmatter, not phase labels or prose descriptions. If all dependencies are satisfied, say so explicitly.

## This pattern is shared

The same frontmatter schema is used across:
- `tender` (this repo) — uses `completed/` for archives
- `rick--edge-platform` — uses `frontlog/done/` for archives
- `rick--machine-learning`
- `rick--starling-edr`

File layout varies per repo; the frontmatter contract is what's shared.
