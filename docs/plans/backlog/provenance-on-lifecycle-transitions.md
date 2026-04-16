---
id: provenance-on-lifecycle-transitions
depends_on: []
links: []
---

# Provenance On Lifecycle Transitions

Distinguish directly observed lifecycle transitions from inferred ones by recording provenance and evidence alongside lifecycle writes.

## Why

Tender already has both direct and inferred lifecycle conclusions.

- direct: the sidecar observes child exit and writes the terminal state
- inferred: reconciliation concludes `SidecarLost` from released lock plus missing terminal write

Today those conclusions are surfaced through the same status shape. That is cheap for the writer but expensive for debugging and downstream tooling.

## Goal

Add provenance to lifecycle writes without changing lifecycle behavior:

- observed transitions remain observed
- inferred transitions are explicitly labeled
- evidence for inferred writes is preserved in a compact typed form

## Design Direction

Add an additive metadata field to lifecycle state, for example:

```json
{
  "status": "SidecarLost",
  "transition_provenance": {
    "kind": "inferred",
    "evidence": ["lock_released", "non_terminal_meta"]
  }
}
```

Direct sidecar writes use:

```json
{
  "transition_provenance": {
    "kind": "direct",
    "evidence": ["sidecar_write"]
  }
}
```

The exact field names may change, but the distinction between direct and inferred must be explicit.

## Scope

- write provenance/evidence when lifecycle state is written
- preserve current status values and exit-code behavior
- surface the data in `status` output
- optionally add `status --explain` if the extra detail needs a more verbose presentation

## Non-Goals

- changing the lifecycle state machine
- changing reconciliation behavior
- introducing confidence scoring yet
- rewriting old session metadata

## Acceptance Criteria

- direct sidecar lifecycle writes are labeled as direct
- `SidecarLost` reconciliation writes are labeled as inferred
- the evidence behind inferred writes is visible in `status` output
- no existing lifecycle behavior changes
