# Project Status

## Current Phase

**Complete.** Implementation and audit remediation finished across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms.

## Issues (2026-03-21)

95 findings from full project audit organized into 16 co-fix groups in `docs/ISSUES.md`, ordered by impact.

- **Group 1 resolved** (8 critical): test trustworthiness — silent skips removed, tautological tests fixed
- **Group 2 resolved** (5 high): sandbox safety bugs — waitpid retry, sentinel preservation, builder error propagation, prctl check, cleanup_stale sequencing
- Remaining: 7 high, 39 medium, 36 low across Groups 3–16
- Group 3 is next highest impact: core untested code (4 high)

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
