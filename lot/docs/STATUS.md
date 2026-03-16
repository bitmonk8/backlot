# Project Status

## Current Phase

**All phases complete.** The library is fully implemented across all three platforms.

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
- Linux backend: user/mount/pid/net/ipc namespaces + seccomp-BPF syscall filtering + cgroups v2 resource limits
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

1. **Fix Linux CI environment.** 8 test failures: 3 cgroup (delegation mismatch) + 5 namespace (`EPERM`). Requires fixing GHA runner setup for user namespaces and cgroup delegation.

2. **First real-world usage / `lot run` testing.** The CLI and library are complete but haven't been exercised end-to-end outside unit/integration tests.

## CI Failure Overview (as of 2026-03-16, run 23150329216)

### macOS — All green

No failures. Tests, clippy, and format all pass.

### Linux — 8 test failures + 1 build error

| Category | Tests | Error | Root Cause |
|---|---|---|---|
| `unexpected_cfgs` (1) | Build + Clippy | `#[cfg(feature = "tracing")]` in `cgroup.rs` references undeclared feature | Dead code referencing nonexistent `tracing` feature. **Fixed**: removed dead tracing block. |
| cgroup tests (3) | `cgroup_guard_creates_and_cleans_up`, `cgroup_guard_add_process`, `cgroup_guard_no_limits_creates_empty` | `cgroups v2 must be available for this test` | CI creates `/sys/fs/cgroup/lot-test` but the test process runs in its own cgroup outside that subtree. The delegated subtree is not the process's current cgroup, so `available()` returns false. |
| namespace spawn tests (5) | `spawn_echo_hello`, `spawn_pid1_in_namespace`, `spawn_proc_mounted`, `spawn_network_isolated`, `spawn_cannot_see_host_paths` | `Setup("child namespace setup failed: Operation not permitted (os error 1)")` | `unshare()` returns `EPERM`. CI runs `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` but the GHA runner may have additional restrictions (AppArmor profile or seccomp filter on the runner process itself) that block namespace creation. |

### Windows — All green

`lot setup` in CI resolved the 5 AppContainer test failures (NUL device ACE). Verified in run 23150329216.

### All jobs

| Job | Status |
|---|---|
| Format | Pass |
| Build | Fail (`unexpected_cfgs` — fixed in b36178a) |
| Clippy (macOS) | Pass |
| Clippy (Linux) | Fail (`unexpected_cfgs` — fixed in b36178a) |
| Clippy (Windows) | Pass |
| Test (macOS) | Pass |
| Test (Linux) | Fail (8 tests — namespace/cgroup CI setup issues) |
| Test (Windows) | Pass |

## Known Limitations

- `max_cpu_seconds` is not enforced via cgroups on Linux (cgroupv2 `cpu.max` controls bandwidth, not total time). Enforced on Windows (Job Objects) and macOS (`RLIMIT_CPU`).
- macOS `mach-lookup` is unrestricted in Seatbelt profiles (narrowing breaks most programs).
- Linux namespace tests require `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+.
- Linux cgroup tests require a delegated subtree writable by the test user.
- Windows: AppContainer processes need a one-time elevated setup for NUL device access and system directory traverse ACEs (see `grant_appcontainer_prerequisites()`). For user-owned directories, `spawn()` grants traverse ACEs automatically at spawn time.
