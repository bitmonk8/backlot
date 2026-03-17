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
- Windows backend: AppContainer (filesystem/network isolation) + Job Objects (resource limits) + sentinel file ACL recovery + deny ACEs for denied paths
- Linux backend: user/mount/pid/net/ipc namespaces + seccomp-BPF syscall filtering + cgroups v2 resource limits (sibling cgroup model) + empty tmpfs overmounts for denied paths + `close_range` fd cleanup to prevent ETXTBSY in parallel spawns
- macOS backend: Seatbelt (sandbox_init SBPL profiles) + setrlimit resource limits + process group kill (setsid/killpg) + ancestor directory `file-read-metadata` grants + SBPL deny rules for denied paths
- CI pipeline: clippy + test on Linux/macOS/Windows with namespace and cgroup setup, `lot setup` in Windows CI
- Rustdoc on all public API items

## Next Work

Full project audit completed (see `docs/AUDIT_FINDINGS.md`). 32 findings remaining: 0 critical, 9 high, 13 medium, 10 low.

### 1. ~~Fix critical issues~~ (Done)

- ~~**Silent cgroup failure drops resource limits.**~~ Fixed: `spawn()` now returns `SandboxError::Setup` when cgroup creation or join fails and `ResourceLimits::has_any()` is true. Cgroup join failure in the helper now reports via `helper_bail!`. If no limits are requested, cgroup failure is still silently ignored.
- ~~**PID recycling race in cgroup `kill_all()` fallback.**~~ Fixed: `kill_all()` fallback now verifies each PID's cgroup membership via `/proc/{pid}/cgroup` before sending SIGKILL. Narrow TOCTOU window remains but the unbounded race is eliminated.

### 2. Fix high issues

- **Non-UTF-8 paths silently skipped in mount namespace.** `namespace.rs:123-147` drops paths that fail `to_str()`. Fix: return an error instead of silently skipping.
- **`setup_stdio_pipes` leaks fds on partial failure.** `unix.rs:150-188` creates up to 6 fds; if later pipes fail, earlier ones leak. Fix: add cleanup on error paths.
- **System dirs always mounted regardless of policy.** `namespace.rs:98-120` bind-mounts `/lib`, `/usr/lib`, `/bin`, `/usr/bin`, `/etc` unconditionally, exposing a broad attack surface. Fix: make system dir mounts configurable or restrict `/etc` to specific needed files.
- **`wait(&self)` race via `Cell<bool>`.** Linux and macOS child types use `Cell<bool>` for `waited` with `wait()` taking `&self`, allowing concurrent waitpid race. Fix: use `&mut self` or atomic flag.
- **No seccomp on aarch64 Linux.** `seccomp.rs` is gated on `#[cfg(target_arch = "x86_64")]`. Fix: add aarch64 syscall table.
- **`max_cpu_seconds` silently ignored on Linux.** No API-level documentation warns callers. Fix: document in `ResourceLimits` and consider returning an error or warning when set on Linux.

### 3. Remaining work

- Fix medium/low audit findings (see `docs/AUDIT_FINDINGS.md`)
- First real-world usage / `lot run` testing

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
