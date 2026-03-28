# Windows Full Backend

Complete the Windows platform implementation started in Phase 2A.

## Scope

- CreateProcess + DETACHED_PROCESS + Job Objects for child spawn
- Windows stdin via `\\.\pipe\tender-<session>`
- Job Object termination for tree kill
- Graceful kill via named stop event → wait → TerminateJobObject
- Windows sidecar spawn + readiness via CreatePipe
- Windows CI (cross-compile + test on GitHub Actions)

## Depends On

Nothing — Phase 2A skeleton and Platform trait already exist.

## Status

Skeleton in `src/platform/windows.rs` — identity + status methods implemented, rest returns Unsupported.
