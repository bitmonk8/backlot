# Project Status

## Current Phase

**All phases complete.** The library is fully implemented across all three platforms. All tests pass on all platforms.

## What Exists

- Workspace structure: `lot/` (library crate) + `lot-cli/` (CLI binary crate)
- Full public API: `spawn()`, `probe()`, `cleanup_stale()`, `SandboxedChild`, `SandboxPolicy`, `SandboxPolicyBuilder`, `SandboxCommand`, `SandboxStdio`, `PlatformCapabilities`, `SandboxError`, `ResourceLimits`, `grant_appcontainer_prerequisites()`, `appcontainer_prerequisites_met()`, `grant_appcontainer_prerequisites_for_policy()`, `appcontainer_prerequisites_met_for_policy()`, `is_elevated()`
- CLI commands: `lot run` (YAML config), `lot setup` (platform prerequisites), `lot probe` (capabilities)
- Policy validation with path canonicalization, overlap detection, deny path coverage checks, resource limit checks
- `SandboxPolicyBuilder`: auto-canonicalization, overlap deduction, platform default paths (exec, lib, temp), deny path support
- `SandboxCommand::forward_common_env()`: forwards 17 standard env vars from parent
- `SandboxedChild::kill_and_cleanup()`: explicit kill + synchronous platform cleanup
- `SandboxedChild::wait_with_output_timeout()`: async timeout with kill+cleanup (behind `tokio` feature)
- `SandboxPolicy` fields are private; construction via `SandboxPolicyBuilder`, access via getter methods
- `SandboxedChild::kill()` and `kill_and_cleanup()` take `&mut self`
- Windows backend: AppContainer (filesystem/network isolation) + Job Objects (resource limits) + sentinel file ACL recovery + deny ACEs for denied paths. Modules: `appcontainer`, `sentinel`, `pipe`, `cmdline`, `job`, `nul_device`, `traverse_acl`, `elevation`, `acl_helpers`, `prerequisites`
- Linux backend: user/mount/pid/net/ipc namespaces + seccomp-BPF syscall filtering (argument-filtered prctl/ioctl) + cgroups v2 resource limits (sibling cgroup model) + empty tmpfs overmounts for denied paths + `close_range` fd cleanup to prevent ETXTBSY in parallel spawns
- macOS backend: Seatbelt (sandbox_init SBPL profiles) + setrlimit resource limits + process group kill (setsid/killpg) + ancestor directory `file-read-metadata` grants + SBPL deny rules for denied paths
- CI pipeline: clippy + test on Linux/macOS/Windows with namespace and cgroup setup, `lot setup` in Windows CI
- Rustdoc on all public API items

## Completed Improvements

- `env_check.rs` module: extracted env/path validation from `lib.rs` with pre-canonicalization, fixed partial canonicalization fallback, delegated `kill_by_pid` to platform modules
- Windows ACL infrastructure: RAII wrappers, shared DACL primitives, unified ACE checks, TOCTOU fix, prerequisites API extraction
- Unix SandboxedChild lifecycle deduplication: extracted `UnixSandboxedChild` struct and `child_bail` function to `unix.rs`, `KillStyle` enum parameterizes kill behavior, `try_wait` compare_exchange moved before `waitpid`, `Drop` sets `waited=true` after reaping, double-wait prevention test added

## Next Work

- First real-world usage / `lot run` testing
- Remaining issues tracked in `docs/ISSUES.md`


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
- Linux deny paths: `stat()` succeeds on denied paths (shows empty tmpfs metadata). macOS/Windows deny `stat()`. Documented cross-platform inconsistency; security guarantee holds on all platforms.
- Linux kernels < 5.9 lack `close_range`; parallel `lot::spawn()` calls from multi-threaded processes may hit ETXTBSY on those kernels. The `close_range` fd cleanup in `close_inherited_fds` mitigates this on 5.9+; on older kernels the race remains possible but spawn still works.
