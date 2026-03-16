# Linux Namespace CI Failures — Diagnostic Findings

## Summary

The 5 namespace test failures on GitHub Actions Linux runners are caused by a **code bug**: `/proc` is mounted in the helper process before the inner fork, but `mount("proc", ...)` requires the calling process to be inside the new PID namespace. Since `unshare(CLONE_NEWPID)` only takes effect after `fork()`, the helper is still in the old PID namespace when it tries to mount `/proc`, and the kernel returns `EPERM`.

This is **not** an environment restriction. The GHA runner environment supports all the namespace operations Lot needs.

## Root Cause

### The Bug

In `lot/src/linux/mod.rs`, the spawn sequence is:

1. Helper process calls `unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWIPC [| CLONE_NEWNET])`
2. Helper calls `setup_user_namespace()` — writes uid_map/gid_map — **succeeds**
3. Helper calls `setup_mount_namespace()` — sets up tmpfs, bind mounts, mounts `/proc`, pivot_root
4. Helper forks inner child (which becomes PID 1 in the new PID namespace)

The problem is step 3: `setup_mount_namespace()` calls `mount_proc()` which does `mount("proc", target, "proc", ...)`. This fails because:

- `unshare(CLONE_NEWPID)` does **not** move the calling process into the new PID namespace. It only affects future children created by `fork()`.
- The kernel requires the caller of `mount("proc", ...)` to be inside the PID namespace that owns the proc mount.
- The helper is still in the **parent's** PID namespace at this point.

### What Works

All other mount operations succeed on GHA runners:

| Operation | Result |
|---|---|
| `mkdir_p` (create mount point) | OK |
| `mount("tmpfs", ...)` | OK |
| `mount(MS_REC \| MS_PRIVATE, "/")` | OK |
| Bind-mount `/lib*`, `/usr/lib*` (exec) | OK |
| Bind-mount `/bin`, `/usr/bin`, `/sbin`, `/usr/sbin` (exec) | OK |
| Bind-mount `/etc` (read-only) | OK |
| Bind-mount policy read paths (read-only) | OK |
| `mount("proc", ...)` | **EPERM** |

### Why It Worked Locally But Not in CI

This bug likely manifested differently depending on the kernel version and configuration. Some kernels may have been more permissive about mounting `/proc` from outside the PID namespace, or local testing may have been done with root privileges (which bypass the check).

## Diagnostic Evidence

### CI Run Data (ubuntu-latest, kernel 6.14.0-1017-azure)

```
[mount-ns] mkdir_p OK
[mount-ns] mount_tmpfs OK
[mount-ns] make_mount_private(/) OK
[mount-ns] make_mount_private(new_root) OK
[mount-ns] essential dirs OK
[mount-ns] lib bind-mounts OK
[mount-ns] bin bind-mounts OK
[mount-ns] /etc bind-mount OK
[mount-ns] policy read paths OK
[mount-ns] FAIL mount_proc: Operation not permitted (os error 1)
```

### Environment State (no restrictions blocking us)

| Check | Value |
|---|---|
| OS | Ubuntu 24.04.3 LTS |
| Kernel | 6.14.0-1017-azure |
| AppArmor profile | `unconfined` |
| Seccomp | `0` (disabled) |
| Seccomp_filters | `0` |
| NoNewPrivs | `0` |
| CapBnd | `000001ffffffffff` |
| `kernel.apparmor_restrict_unprivileged_userns` | `0` (after sysctl) |
| `kernel.unprivileged_userns_clone` | `1` (after sysctl) |
| `unshare(CLONE_NEWUSER)` from forked child | succeeds |
| uid_map/gid_map write | succeeds |
| Combined unshare (all flags) + uid_map | succeeds |

Same results on ubuntu-22.04 (kernel 6.8.0-1044-azure).

### Initial Misdirection

The original diagnosis assumed `unshare(CLONE_NEWUSER)` was blocked by AppArmor or a seccomp filter on the GHA runner. This was based on the generic error message `"child namespace setup failed: Operation not permitted (os error 1)"` which did not identify which step failed.

After adding step-based error reporting, the error was pinpointed to the mount namespace setup, and then to the specific `mount_proc()` call.

### AppArmor Interaction (ubuntu-24.04 only)

Ubuntu 24.04 has `kernel.apparmor_restrict_unprivileged_userns=1` by default. When enabled, AppArmor transitions processes that create user namespaces to the `unprivileged_userns` profile, which denies `CAP_SYS_ADMIN`. This blocks `unshare(CLONE_NEWUSER)` at the AppArmor level.

Setting `kernel.apparmor_restrict_unprivileged_userns=0` disables this transition. After the sysctl change, all namespace operations (including `unshare(CLONE_NEWUSER)` and uid_map writes) succeed.

Ubuntu 22.04 does not have this sysctl — AppArmor does not restrict user namespaces there.

**This AppArmor issue is orthogonal to the mount_proc bug.** Even after fixing AppArmor, the proc mount fails. The AppArmor sysctl is only needed on 24.04+ and the CI already sets it.

## Fix

Move `mount_proc()` to execute **after** the inner fork (in the inner child process, which is PID 1 inside the new PID namespace).

Two possible approaches:

### Option A: Split `setup_mount_namespace()` into two phases

1. Phase 1 (in helper, before inner fork): tmpfs, bind mounts, essential dirs, dev nodes, pivot_root — everything except `/proc`
2. Phase 2 (in inner child, after fork): mount `/proc`

Trade-off: Simpler change, but the inner child does mount work after pivot_root which means the proc mount path needs to use the new root's path.

### Option B: Move entire mount setup after fork

Do all mount namespace setup in the inner child (PID 1). The inner child inherits the mount namespace from the helper.

Trade-off: Cleaner separation but more code restructuring. The helper only does unshare + uid_map, everything else happens in the inner child.

### Option C: Bind-mount host `/proc` instead of mounting a new procfs

Instead of `mount("proc", target, "proc", ...)`, bind-mount `/proc` read-only from the host. This avoids the PID namespace requirement.

Trade-off: Exposes the host's `/proc` (all PIDs visible), which weakens isolation. Not recommended.

### Recommendation

**Option A** is the smallest fix. The proc mount should happen in the inner child after fork. Since `pivot_root` has already executed in the helper, the inner child needs to mount proc at `/proc` (relative to the new root, which is now `/`).

Specifically:
1. Remove `mount_proc()` from `setup_mount_namespace()`
2. In the inner child (after fork, after dup2/chdir, before seccomp), call `mount_proc("/proc")`

## CI Changes Made During Investigation

The following diagnostic changes were added during this investigation:

1. **Diagnostic CI steps**: Environment probes (AppArmor, seccomp, capabilities, sysctl values, strace, dmesg)
2. **ubuntu-22.04 test job**: Parallel test matrix to compare runner images
3. **`diagnose_namespace_steps` test**: Step-by-step namespace setup verification
4. **`diagnose_namespace_combined_flags` test**: Combined unshare flags verification
5. **Step-based error pipe**: `spawn()` now reports which step failed (unshare, user_ns, mount_ns, fork, dup2, chdir, seccomp, exec)
6. **Raw write(2) diagnostics**: Mount namespace setup prints progress to stderr in test builds

These diagnostic additions should be cleaned up after the fix is applied. The step-based error reporting in the error pipe is worth keeping permanently.
