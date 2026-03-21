# Project Status

## Current Phase

**Issue remediation.** Implementation complete across all platforms. Working through audit findings in `docs/ISSUES.md`.

## Goal

Clear all issues in `docs/ISSUES.md`.

## Next Work

**Group 2: Policy Validation & Path Safety**

## Completed

- Full implementation (Linux, macOS, Windows)
- CI pipeline (format, clippy, build, test on all 3 platforms)
- Group 1: Sandbox Enforcement Correctness (1.1 kill_and_reap deadlock, 1.2 bind_mount submounts, 1.3 escape_sbpl_path)
- Group 3: CI Test Reliability — Silent Skips (3.1, 3.2, 3.3)
- Group 4.1: Post-fork `_exit` fix
- Group 4.2: Test zombie process fix
- Group 5.1: unix.rs unit tests (14 tests)
- Group 5.3: Integration test try_wait loop bounded
- Group 8: Platform-Specific Test Gaps (8.1–8.7)

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
