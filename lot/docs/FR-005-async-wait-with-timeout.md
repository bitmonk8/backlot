# FR-005: `wait_with_output_timeout` (Feature-Gated Async)

## Problem

`SandboxedChild::wait_with_output()` is synchronous and blocking. Async consumers (tokio, async-std) must:

1. Wrap the call in `spawn_blocking`.
2. Race it against `tokio::time::timeout`.
3. On timeout, kill the child by PID to unblock `wait_with_output` and close pipes.
4. Handle the detached `JoinHandle` — `SandboxedChild::drop` runs on the background thread, but may not complete if the process exits.

Epic has ~30 lines implementing this pattern (`wait_for_output`, `kill_process`). The timeout-kill-cleanup sequence is subtle and easy to get wrong.

## Proposed Solution

Feature-gated async method:

```rust
// Behind `features = ["tokio"]` in Cargo.toml
impl SandboxedChild {
    /// Wait for the child to exit with a timeout. On timeout, kills the
    /// child (and all descendants), runs platform cleanup, and returns
    /// a timeout error.
    pub async fn wait_with_output_timeout(
        self,
        timeout: std::time::Duration,
    ) -> Result<std::process::Output, SandboxError>;
}
```

Internally:
1. Moves `self` into `spawn_blocking`.
2. Races against the timeout.
3. On timeout, kills the child and descendants (using `kill_and_cleanup` from FR-002 or equivalent internal logic).
4. Awaits the `spawn_blocking` handle to ensure cleanup completes before returning.

This eliminates the detached-handle cleanup problem (FR-002's primary motivator for async consumers).

## Dependencies

- `tokio` (feature-gated, not a default dependency)
- FR-001 (macOS descendant kill) for correct cross-platform timeout behavior
- FR-002 (`kill_and_cleanup`) or equivalent internal cleanup path

## Scope

New module behind `#[cfg(feature = "tokio")]`. Adds optional `tokio` dependency.
