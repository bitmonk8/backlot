# Project Status

## Current Phase

**Complete.** Implementation and audit remediation finished across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms.

## Issues (2026-03-21)

95 findings from full project audit organized into 16 co-fix groups in `docs/ISSUES.md`, ordered by impact.

- **Group 1 resolved** (8 critical): test trustworthiness — silent skips removed, tautological tests fixed
- Remaining: 12 high, 39 medium, 36 low across Groups 2–16
- Groups 2–3 are next highest impact: sandbox safety bugs (5 high), core untested code (4 high)

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
