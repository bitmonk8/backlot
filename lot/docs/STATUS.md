# Project Status

## Current Phase

**All phases complete.** The library is fully implemented across all three platforms.

## What Exists

- Full public API: `spawn()`, `probe()`, `cleanup_stale()`, `SandboxedChild`, `SandboxPolicy`, `SandboxPolicyBuilder`, `SandboxCommand`, `SandboxStdio`, `PlatformCapabilities`, `SandboxError`, `ResourceLimits`, `grant_appcontainer_prerequisites()`, `appcontainer_prerequisites_met()`, `grant_appcontainer_prerequisites_for_policy()`, `appcontainer_prerequisites_met_for_policy()`, `is_elevated()`
- Policy validation with path canonicalization, overlap detection, resource limit checks
- `SandboxPolicyBuilder`: auto-canonicalization, overlap deduction, platform default paths (exec, lib, temp)
- `SandboxCommand::forward_common_env()`: forwards 17 standard env vars from parent
- `SandboxedChild::kill_and_cleanup()`: explicit kill + synchronous platform cleanup
- `SandboxedChild::wait_with_output_timeout()`: async timeout with kill+cleanup (behind `tokio` feature)
- Windows backend: AppContainer (filesystem/network isolation) + Job Objects (resource limits) + sentinel file ACL recovery
- Linux backend: user/mount/pid/net/ipc namespaces + seccomp-BPF syscall filtering + cgroups v2 resource limits
- macOS backend: Seatbelt (sandbox_init SBPL profiles) + setrlimit resource limits + process group kill (setsid/killpg)
- CI pipeline: clippy + test on Linux/macOS/Windows with namespace and cgroup setup
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

### AppContainer Ancestor Traverse ACEs (Windows)

Design: `docs/DESIGN_SPAWN_TRAVERSE_ACES.md`

AppContainer sandboxed processes cannot call `fs::metadata()` on ancestor directories of policy paths because those directories lack an ACE for `ALL APPLICATION PACKAGES`. This breaks NuShell glob traversal, `create_dir_all`, and similar patterns.

| Task | Status |
|---|---|
| `grant_appcontainer_prerequisites(paths)` — one-time elevated setup granting traverse ACEs on ancestors + NUL device | Complete |
| `appcontainer_prerequisites_met(paths)` — check all ancestors + NUL device | Complete |
| `is_elevated()` — replaces `can_modify_nul_device()` | Complete |
| `compute_ancestors(paths)` internal helper | Complete |
| `grant_traverse(path)` / `has_traverse_ace(path)` internals | Complete |
| Remove `grant_nul_device_access()`, `can_modify_nul_device()` | Complete |

### Best-Effort Spawn-Time Traverse ACE Grant (Windows)

Design: `docs/DESIGN_SPAWN_TRAVERSE_ACES.md`

Automatically grant ancestor traverse ACEs inside `spawn_inner()` before creating the sentinel. Grants the ACE unconditionally (idempotent, harmless on directories already traversable via other ACEs). Only fails for system directories the current user cannot modify, returning `SandboxError::PrerequisitesNotMet`.

| Task | Status |
|---|---|
| `compute_ancestors` generic over `AsRef<Path>` — accepts both `&Path` and `PathBuf` slices | Complete |
| Insert best-effort grant loop in `spawn_inner()` (after `all_paths`, before `write_sentinel`) | Complete |
| Return `SandboxError::PrerequisitesNotMet` on grant failure or missing NUL device | Complete |
| Integration tests | Complete |

## Next Work

1. **Fix CI prerequisite setup.** Tests no longer silently skip when prerequisites are missing — they fail. CI must be configured to provide the required environment on each platform before running tests.

2. **Fix test bugs exposed by silent-skip removal.** Fix any test failures that occur even when prerequisites are met.

## CI Failure Overview (as of 2026-03-16)

Tests previously silently skipped on failure; now they fail loudly. This exposed missing CI setup and pre-existing bugs.

### macOS — All green

No failures. Seatbelt works without special setup.

### Linux — 8 test failures

| Category | Tests | Error | Root Cause |
|---|---|---|---|
| cgroup tests (3) | `cgroup_guard_creates_and_cleans_up`, `cgroup_guard_add_process`, `cgroup_guard_no_limits_creates_empty` | `cgroups v2 must be available for this test` | CI creates `/sys/fs/cgroup/lot-test` but the test process runs in its own cgroup outside that subtree. The delegated subtree is not the process's current cgroup, so `available()` returns false. |
| namespace spawn tests (5) | `spawn_echo_hello`, `spawn_pid1_in_namespace`, `spawn_proc_mounted`, `spawn_network_isolated`, `spawn_cannot_see_host_paths` | `Setup("child namespace setup failed: Operation not permitted (os error 1)")` | `unshare()` returns `EPERM`. CI runs `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` but the GHA runner may have additional restrictions (AppArmor profile or seccomp filter on the runner process itself) that block namespace creation. |
| integration tests (7) | All integration tests | Same namespace `EPERM` | Same root cause as namespace spawn tests above. |

### Windows — 5 unit test failures + 7 integration test failures

| Category | Tests | Error | Root Cause |
|---|---|---|---|
| appcontainer unit tests (5) | `spawn_and_read_allowed_path`, `disallowed_path_unreadable`, `read_only_path_not_writable`, `cleanup_restores_acls`, `sentinel_recovery` | `PrerequisitesNotMet { nul_device_missing: true }` (first test), then cascading `PoisonError` | NUL device lacks `ALL APPLICATION PACKAGES` ACE. CI has no setup step to call `grant_appcontainer_prerequisites()`. The GHA Windows runner is elevated, so adding a setup step that calls this function should fix it. The cascading `PoisonError` is because the tests share a `Mutex` and the first panic poisons it. |
| integration tests (7) | All integration tests | `PrerequisitesNotMet { missing_paths: ["\\\\?\\C:\\Users"] }` | Same missing prerequisites. Ancestor traverse ACEs not granted for temp dir paths under `C:\Users`. |

### All platforms — format failure

| Category | Error | Root Cause |
|---|---|---|
| `cargo fmt --check` | Diff in `src/linux/mod.rs` | Long `.expect()` chains need line-wrapping per `rustfmt` rules. |

## Known Limitations

- `max_cpu_seconds` is not enforced via cgroups on Linux (cgroupv2 `cpu.max` controls bandwidth, not total time). Enforced on Windows (Job Objects) and macOS (`RLIMIT_CPU`).
- macOS `mach-lookup` is unrestricted in Seatbelt profiles (narrowing breaks most programs).
- Linux namespace tests require `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+.
- Linux cgroup tests require a delegated subtree writable by the test user.
- Windows: AppContainer processes need a one-time elevated setup for NUL device access and system directory traverse ACEs (see `grant_appcontainer_prerequisites()`). For user-owned directories, `spawn()` grants traverse ACEs automatically at spawn time.
