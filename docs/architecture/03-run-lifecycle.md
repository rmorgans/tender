# Run Lifecycle

Tender models supervised runs, not raw processes. The sidecar is the normal writer of lifecycle state. The CLI only writes lifecycle state in one reconciliation case: `SidecarLost`.

```mermaid
stateDiagram-v2
    [*] --> Starting: tender start / tender run\nspawn detached sidecar

    Starting --> Running: sidecar spawns child\nand writes Running
    Starting --> SpawnFailed: child spawn fails
    Starting --> DependencyFailed: --after wait fails / times out / is killed
    Starting --> SidecarLost: CLI reconciliation\n(lock released, no terminal state)

    Running --> ExitedOk: child exits 0
    Running --> ExitedError: child exits non-zero
    Running --> Killed: graceful kill classified by sidecar
    Running --> KilledForced: forced kill classified by sidecar
    Running --> TimedOut: timeout thread fires
    Running --> SidecarLost: CLI reconciliation\n(lock released, no terminal state)

    ExitedOk --> [*]
    ExitedError --> [*]
    Killed --> [*]
    KilledForced --> [*]
    TimedOut --> [*]
    SpawnFailed --> [*]
    DependencyFailed --> [*]
    SidecarLost --> [*]
```

Authority rules:

This ownership boundary follows Theme 2: One Authority Per Fact; see [../design-principles.md](../design-principles.md).

- Normal lifecycle writes happen in the sidecar:
  - `Starting -> Running`
  - `Starting -> SpawnFailed`
  - `Starting -> DependencyFailed`
  - `Running -> Exited* / Killed* / TimedOut`
- `SidecarLost` is the only reconciliation write performed outside the sidecar:
  - `status`
  - foreground `run`

Important implementation detail:

- dependency waits happen while the run is still in `Starting`
- if `--after` is present, the sidecar writes `Starting` and signals readiness before it begins polling dependencies
- dependency binding is by `(session, run_id)`, so `--replace` on a dependency causes the waiter to fail rather than silently following a different execution

What this diagram omits:

- the OS-specific mechanics of kill, wait, and process identity
- PTY control, which is separate from run lifecycle and covered in [04-pty-lane.md](04-pty-lane.md)
