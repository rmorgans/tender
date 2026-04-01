---
id: fleet-migration
depends_on:
  - remote-ssh-transport
links: []
---

# Fleet Migration — atch to tender

Migrate production fleet from `atch` to `tender`.

## Goal

Replace `atch` across the managed fleet without losing observability, session control, or rollback safety.

This is an operational rollout plan, not a product implementation plan.

## First Slice Goal

Prepare the fleet for cutover before any host is switched.

First-slice deliverables:

- migration guide from `atch` to `tender`
- command mapping table
- host inventory and rollout order
- validation checklist per host
- rollback procedure

Actual cutover remains blocked on `remote-ssh-transport`, but the planning and operator docs should be done ahead of time.

## Migration Phases

1. **Readiness**
   - identify target hosts
   - document current `atch` workflows in use
   - define success metrics and rollback triggers
2. **Migration Guide**
   - map `atch` commands to `tender` equivalents
   - document behavioral differences
   - document storage and log layout differences
3. **Parallel Install**
   - install `tender` alongside `atch`
   - verify local and remote invocation paths
   - exercise representative sessions without cutting over hooks
4. **Cutover**
   - switch agent hooks, scripts, and tooling from `atch` to `tender`
   - monitor for regressions and operator pain points
5. **Cleanup**
   - remove `atch`
   - archive migration notes and host status

## Required Deliverables

- fleet inventory and rollout order
- `atch` to `tender` command mapping doc
- per-host validation checklist
- rollback procedure
- final cutover checklist
- completion tracker for each host or host cohort

## Behavioral Differences To Document

The guide should explicitly call out:

- structured JSON / NDJSON output vs human-oriented output
- session naming and namespace semantics
- idempotent `start` behavior
- `run` vs raw shell script invocation
- `wrap` as the hook integration path
- Windows behavior where relevant
- differences in kill / timeout / replace semantics if operators relied on `atch` behavior

## Rollout Strategy

The cutover should be host-by-host or cohort-by-cohort, not global.

Recommended order:

1. canary host(s)
2. low-risk cohort
3. standard fleet cohort
4. high-sensitivity hosts last

Each cohort should have:

- explicit owner
- validation checklist
- rollback trigger
- recorded completion status

## Validation Checklist

Each migrated host should validate at least:

- `tender start`
- `tender status`
- `tender log`
- `tender watch`
- `tender push` for stdin-enabled sessions
- `tender kill`
- `tender wait`
- `tender run`
- hook path using `tender wrap` if applicable

## Rollback Requirements

Rollback must be documented before the first cutover.

Minimum rollback plan:

- revert hook/script invocations from `tender` back to `atch`
- preserve or archive any in-flight Tender session data if needed
- restore previous install or symlink state
- define the operator decision point for aborting a cohort rollout

## Implementation Tasks

1. Produce the migration guide and command mapping table
2. Identify fleet hosts, cohorts, and rollout order
3. Define per-host validation steps
4. Define rollback steps back to `atch`
5. Prepare a pilot cohort before broad cutover
6. Add a simple migration tracker so each host has a recorded state

## Acceptance Criteria

- every host has an explicit migration status
- a documented rollback exists before the first cutover
- hooks and scripts have an audited `atch` to `tender` mapping
- cutover can be performed incrementally rather than all at once
- remote operations are validated through the supported SSH transport once it lands

## Depends On

`remote-ssh-transport` blocks actual fleet cutover because remote fleet management depends on `tender --host`. The migration guide and rollout prep do not need to wait.

## Not In Scope

- the Claude Code skill (separate plan: `skill-claude-code`)
- Tender feature development
- replacing every non-supervision use case of `atch` if it falls outside Tender's product scope
