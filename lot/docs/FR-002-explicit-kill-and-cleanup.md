# FR-002: Explicit `kill_and_cleanup()` Method

## Problem

`SandboxedChild::drop` performs cleanup (ACL restoration on Windows, cgroup removal on Linux). When a consumer wraps `wait_with_output()` in a timeout and the timeout fires, the `SandboxedChild` is inside a background thread. The consumer kills the child by PID to unblock the wait, and the `SandboxedChild` eventually drops — but if the process exits before the background thread completes, cleanup is skipped.

On Windows this leaves stale ACL entries on project files. On Linux this leaves orphaned cgroup directories.

## Proposed Solution

Add a public method:

```rust
impl SandboxedChild {
    /// Kill the sandboxed process (and all descendants), then run
    /// platform cleanup synchronously. Returns after cleanup is complete.
    pub fn kill_and_cleanup(self) -> Result<(), SandboxError>;
}
```

This lets the consumer call it from the main thread before returning a timeout error, guaranteeing cleanup runs. The existing `Drop` impl becomes a fallback for callers who don't call this explicitly.

## Relationship to FR-005

If lot provides `wait_with_output_timeout` (FR-005), lot can call `kill_and_cleanup` internally on timeout, making this method unnecessary for that use case. This FR is still useful independently for consumers who manage their own timeout logic.
