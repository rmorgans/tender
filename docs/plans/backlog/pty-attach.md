# PTY Attach — Human Escape Hatch

Let humans take over when agents can't handle it.

## Scope

- PTY support (Unix: forkpty, Windows: ConPTY)
- `tender attach <session>` command
- Detach key handling
- Requires process started with PTY mode

## Depends On

- Remote SSH transport (most useful when combined)

## Notes

Original design spec Phase 5. Lowest priority — agents are the primary consumer.
