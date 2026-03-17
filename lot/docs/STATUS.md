# Project Status

## Current Phase

**All phases complete.** The library is fully implemented across all three platforms. All tests pass on all platforms.

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
- Linux backend: user/mount/pid/net/ipc namespaces + seccomp-BPF syscall filtering + cgroups v2 resource limits (sibling cgroup model)
- macOS backend: Seatbelt (sandbox_init SBPL profiles) + setrlimit resource limits + process group kill (setsid/killpg) + ancestor directory `file-read-metadata` grants
- CI pipeline: clippy + test on Linux/macOS/Windows with namespace and cgroup setup, `lot setup` in Windows CI
- Rustdoc on all public API items

## Next Work

1. **First real-world usage / `lot run` testing.** The CLI and library are complete but haven't been exercised end-to-end outside unit/integration tests.

## CI Status

| Job | Status |
|---|---|
| Format | Pass |
| Build | Pass |
| Clippy (macOS) | Pass |
| Clippy (Linux) | Pass |
| Clippy (Windows) | Pass |
| Test (macOS) | Pass |
| Test (Linux) | Pass |
| Test (Windows) | Pass |

### Note: ubuntu-24.04 AppArmor

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.

## Known Limitations

- `max_cpu_seconds` is not enforced via cgroups on Linux (cgroupv2 `cpu.max` controls bandwidth, not total time). Enforced on Windows (Job Objects) and macOS (`RLIMIT_CPU`).
- macOS `mach-lookup` is unrestricted in Seatbelt profiles (narrowing breaks most programs).
- Linux namespace tests require `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+.
- Linux cgroup tests require a delegated subtree with `+memory +pids` in `subtree_control`. CI creates this under the runner's cgroup parent.
- Windows: AppContainer processes need a one-time elevated setup for NUL device access and system directory traverse ACEs (see `grant_appcontainer_prerequisites()`). For user-owned directories, `spawn()` grants traverse ACEs automatically at spawn time.
