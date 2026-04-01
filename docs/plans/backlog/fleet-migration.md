---
id: fleet-migration
depends_on:
  - remote-ssh-transport
links: []
---

# Fleet Migration — atch to tender

Migrate production fleet from atch to tender.

## Scope

1. **Migration guide:** CLI mapping from atch commands to tender equivalents, behavioral differences (structured output vs human text, state machine differences), session storage layout differences
2. **Parallel install:** install tender alongside atch on fleet hosts, validate both work
3. **Cutover:** switch agent hooks/scripts from atch to tender, monitor for regressions
4. **Cleanup:** remove atch from fleet

## Depends On

`remote-ssh-transport` — fleet migration requires `tender --host` for remote management. The migration guide can be written before remote lands, but the actual fleet cutover cannot.

## Not in Scope

- The Claude Code skill (separate plan: `skill-claude-code`)
- tender feature development (this plan is operational, not product)

> **Needs expansion before promotion:** Add specific fleet hosts, atch command mapping table, rollback procedure, and success criteria.
