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

## Status (as of Slice 0, 2026-03-28)

- Skeleton in `src/platform/windows.rs` — identity + status methods implemented, rest returns Unsupported
- Signature-compatible: all 17 test binaries compile on Windows (rustc 1.93.1)
- Backend-stubbed: integration tests fail at spawn_child (expected)
- Pre-existing: 4 session_fs meta read/write tests fail on Windows (path separator or fsync behavior — not gated, needs investigation)
