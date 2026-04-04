# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

69 active findings across 19 groups in `docs/ISSUES.md`.

- **MUST FIX (0)**
- **NON-CRITICAL (0)**
- **NIT (69):** Groups 3–20 — lifecycle test gaps (downgraded from NON-CRITICAL after review: true findings but code works correctly, tests just don't prove specific behaviors), silent cleanup failures, TOCTOU (mitigated), canonicalization fallback, correctness, error handling, incorrect comments, doc mismatches, separation of concerns, architectural simplification, naming, code duplication, minor cleanup, test boilerplate, NIT test coverage gaps

Groups reordered and renumbered by impact (2026-03-24). NON-CRITICAL first, then NIT by category: correctness > error handling > docs > architecture > naming > simplification > testing.

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
