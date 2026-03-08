# FR-001: macOS Descendant Kill on Drop

## Problem

On Linux, `CgroupGuard::drop` kills all descendants of a sandboxed child. On Windows, the job object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) does the same. On macOS, `SandboxedChild::drop` only sends `SIGKILL` to the direct child — grandchildren are reparented to init and continue running.

This means a sandboxed `sh -c "script_that_forks &"` on macOS will leak the forked process after timeout or drop.

## Proposed Solution

Use process groups on macOS:

1. Call `setsid()` (or `setpgid(0, 0)`) in the child before `exec`, so the sandboxed child becomes its own process group leader.
2. In `MacSandboxedChild::drop`, call `killpg(pgid, SIGKILL)` instead of `kill(pid, SIGKILL)` to kill the entire group.

`setsid()` is async-signal-safe and compatible with `posix_spawn` via `POSIX_SPAWN_SETSID` (macOS 10.15+) or the `pre_exec` hook.

## Scope

macOS platform module only. No API change — `SandboxedChild::drop` already kills; this widens the kill to include descendants.
