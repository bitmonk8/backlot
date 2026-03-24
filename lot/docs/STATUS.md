# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

New audit completed. 100 active findings across 21 groups in `docs/ISSUES.md` (7 false positives removed).

- **MUST FIX (3):** Group 1 (unsafe errno in child_bail macro — Linux + macOS), Group 2 items 3–4 (incorrect security-semantic comments)
- **NON-CRITICAL (47):** Groups 3–8, 9–12, 14–15 — silent cleanup failures, missing test coverage, TOCTOU, canonicalization error handling, weak assertions, seccomp/fork error handling, separation of concerns, placement
- **NIT (58):** Groups 10, 13, 16–21 — simplification, naming, test boilerplate, doc mismatches, architectural cleanup

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
