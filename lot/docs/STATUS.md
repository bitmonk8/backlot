# Project Status

## Current Phase

**All phases complete.** The library is fully implemented across all three platforms. One bug remains in the Linux namespace setup.

## What Exists

- Workspace structure: `lot/` (library crate) + `lot-cli/` (CLI binary crate)
- Full public API: `spawn()`, `probe()`, `cleanup_stale()`, `SandboxedChild`, `SandboxPolicy`, `SandboxPolicyBuilder`, `SandboxCommand`, `SandboxStdio`, `PlatformCapabilities`, `SandboxError`, `ResourceLimits`, `grant_appcontainer_prerequisites()`, `appcontainer_prerequisites_met()`, `grant_appcontainer_prerequisites_for_policy()`, `appcontainer_prerequisites_met_for_policy()`, `is_elevated()`
- CLI commands: `lot run` (YAML config), `lot setup` (platform prerequisites), `lot probe` (capabilities)
- Policy validation with path canonicalization, overlap detection, resource limit checks
- `SandboxPolicyBuilder`: auto-canonicalization, overlap deduction, platform default paths (exec, lib, temp)
- `SandboxCommand::forward_common_env()`: forwards 17 standard env vars from parent
- `SandboxedChild::kill_and_cleanup()`: explicit kill + synchronous platform cleanup
- `SandboxedChild::wait_with_output_timeout()`: async timeout with kill+cleanup (behind `tokio` feature)
- Windows backend: AppContainer (filesystem/network isolation) + Job Objects (resource limits) + sentinel file ACL recovery
- Linux backend: user/mount/pid/net/ipc namespaces + seccomp-BPF syscall filtering + cgroups v2 resource limits (sibling cgroup model to respect cgroupv2 "no internal processes" constraint)
- macOS backend: Seatbelt (sandbox_init SBPL profiles) + setrlimit resource limits + process group kill (setsid/killpg)
- CI pipeline: clippy + test on Linux/macOS/Windows with namespace and cgroup setup, `lot setup` in Windows CI
- Rustdoc on all public API items

## Implementation Plan

See `docs/PLAN.md` for the full phased plan with CI testing strategy.

### Phase Summary

| Phase | Scope | Status |
|---|---|---|
| 0 | Policy validation + test infrastructure | Complete |
| 1 | `probe()` implementations (all platforms) | Complete |
| 2 | Windows Job Objects | Complete |
| 3 | Windows AppContainer | Complete |
| 4 | Linux seccomp-BPF | Complete |
| 5 | Linux namespaces + filesystem | Complete |
| 6 | Linux cgroups v2 | Complete |
| 7 | macOS Seatbelt | Complete |
| 8 | Integration + hardening | Complete |

### Post-v1 Features

| Feature | Status |
|---|---|
| macOS descendant kill via setsid/killpg | Complete |
| `kill_and_cleanup()` method | Complete |
| `SandboxPolicyBuilder` with auto-canonicalization | Complete |
| `forward_common_env()` on `SandboxCommand` | Complete |
| `wait_with_output_timeout()` (tokio feature) | Complete |
| Policy-based prerequisites API (`grant_appcontainer_prerequisites_for_policy`, `appcontainer_prerequisites_met_for_policy`) | Complete |
| Spawn-time prerequisite check (`SandboxError::PrerequisitesNotMet`) | Complete — replaced check-only approach with grant-then-check |
| Best-effort spawn-time traverse ACE grant | Complete |
| Workspace restructure + CLI binary | Complete |

Design docs: `docs/DESIGN_WORKSPACE_CLI.md`, `docs/DESIGN_SPAWN_TRAVERSE_ACES.md`

## Next Work

1. **Fix Linux `mount_proc` bug.** The `/proc` mount in `setup_mount_namespace()` runs in the helper process before the inner fork, but mounting `/proc` requires the process to be inside the new PID namespace (which only takes effect after `fork()`). Fix: move `mount_proc()` to the inner child. See `docs/LINUX_NAMESPACE_FAILURES.md` for full analysis.

2. **Clean up diagnostic code.** Remove temporary diagnostic tests and `#[cfg(test)]` `diag()` calls from `namespace.rs`. Keep the step-based error reporting in the error pipe (useful permanently). Remove the ubuntu-22.04 CI job (added only for diagnosis).

3. **First real-world usage / `lot run` testing.** The CLI and library are complete but haven't been exercised end-to-end outside unit/integration tests.

## CI Status (as of 2026-03-16, run 23159265328)

| Job | Status |
|---|---|
| Format | Pass |
| Build | Pass |
| Clippy (macOS) | Pass |
| Clippy (Linux) | Pass |
| Clippy (Windows) | Pass |
| Test (macOS) | Pass |
| Test (Linux) | 5 namespace failures + 2 diagnostic test failures |
| Test (Linux, ubuntu-22.04) | Same failures as ubuntu-latest |
| Test (Windows) | Pass |

### Linux namespace failures — root cause identified

All 5 namespace tests fail at the `mount_proc` step inside `setup_mount_namespace()`. The `mount("proc", ...)` call returns EPERM because:

- `unshare(CLONE_NEWPID)` does not move the calling process into the new PID namespace — only its children (after `fork()`).
- The helper process calls `mount("proc", ...)` before forking the inner child.
- The kernel requires the caller to be inside the PID namespace to mount `/proc`.

This is a **code bug**, not an environment restriction. All other namespace operations (unshare, uid_map, tmpfs, bind mounts, mount propagation) succeed on GHA runners.

See `docs/LINUX_NAMESPACE_FAILURES.md` for complete diagnostic findings, evidence, and fix options.

### Note: ubuntu-24.04 AppArmor

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this. This is orthogonal to the mount_proc bug.

## Known Limitations

- `max_cpu_seconds` is not enforced via cgroups on Linux (cgroupv2 `cpu.max` controls bandwidth, not total time). Enforced on Windows (Job Objects) and macOS (`RLIMIT_CPU`).
- macOS `mach-lookup` is unrestricted in Seatbelt profiles (narrowing breaks most programs).
- Linux namespace tests require `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+.
- Linux cgroup tests require a delegated subtree with `+memory +pids` in `subtree_control`. CI creates this under the runner's cgroup parent.
- Windows: AppContainer processes need a one-time elevated setup for NUL device access and system directory traverse ACEs (see `grant_appcontainer_prerequisites()`). For user-owned directories, `spawn()` grants traverse ACEs automatically at spawn time.
