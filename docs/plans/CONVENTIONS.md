# Docs Conventions

## Work tracking

Work items live in `docs/plans/frontlog/` and `docs/plans/backlog/`. The filesystem is the index.

```
docs/plans/
├── frontlog/          # active queue, sequential: 01_, 02_
│   ├── 01_name.md
│   ├── 02_name.md
│   └── done/          # completed items, date-prefixed
├── backlog/           # deferred queue, gap-numbered or date-slugged
├── completed/         # legacy completed items (pre-convention)
```

### Naming

| Location | Naming | Example |
|----------|--------|---------|
| `frontlog/` | Sequential number or slug | `01_run_shebang.md` or `tender-run-shebang.md` |
| `backlog/` | Gap-numbered or date-slugged | `10_name.md` or `2026-03-14-name.md` |
| `frontlog/done/` | `YYYY-MM-DD_<stable-id>_<slug>.md` | `2026-03-30_run-shebang_supervised_scripts.md` |

Active items use numeric names for prioritisation. Done items use date + stable ID for chronological history with provenance.

### Frontmatter schema

Every work item has YAML frontmatter:

```yaml
---
id: cave-ingest-v3                # semantic, never changes on reorder/archive
title: Short Title
created: 2026-03-27
closed:                           # date when moved to done/
depends_on: []                    # stable ids, e.g. [ingress-key-generation]
links: []                         # relative paths to related docs
---
```

- **No `status:` field.** Directory placement is the status.
- **Semantic ids** — not queue numbers. `run-shebang` not `frontlog-01`.
- **`depends_on`** references stable ids only.
- **One file per item.** Card + implementation plan in the same file.

### Lifecycle

1. New work → create in `frontlog/` with next sequential number
2. Deferred work → create in `backlog/` with gap number or date slug
3. Completed → `git mv` to `frontlog/done/`, rename to `YYYY-MM-DD_<id>_<slug>.md`, set `closed:` date

### Non-work-item archives

Historical docs that were never queue items (old plans, investigations, superseded designs) live in `completed/` (legacy) or topic archives.

## This pattern is shared

The same `frontlog/` + `backlog/` + frontmatter schema is used across:
- `tender` (this repo)
- `rick--edge-platform`
- `rick--machine-learning`
- `rick--starling-edr`
