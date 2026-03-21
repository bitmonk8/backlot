# Project Status

## Current Phase

**Issue remediation.** Implementation complete across all platforms. Working through audit findings in `docs/ISSUES.md`.

## Goal

Clear all issues in `docs/ISSUES.md`.

## Next Work

**Group 6: Windows ACL & Error Handling (6.1–6.5)**

## Completed

- Full implementation (Linux, macOS, Windows)
- CI pipeline (format, clippy, build, test on all 3 platforms)
- Group 1: Sandbox Enforcement Correctness (1.1–1.3)
- Group 2: Policy Validation & Path Safety (2.1–2.8)
- Group 3: CI Test Reliability — Silent Skips (3.1–3.3)
- Group 4: Linux Cgroup Issues (4.1–4.4)
- Group 5: Unix Process Management (5.1–5.3)
- Group 8: Platform-Specific Test Gaps (8.1–8.7)

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
