# Project Status

## Current Phase

**All phases complete.** The library is fully implemented across all three platforms. All tests pass on all platforms.

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
