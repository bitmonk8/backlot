# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

77 active findings across 20 groups in `docs/ISSUES.md`.

- **MUST FIX (0)**
- **NON-CRITICAL (7):** Groups 2–4 — weak assertions, lifecycle test gaps, silent cleanup failures
- **NIT (70):** Groups 5–20 — TOCTOU (mitigated), canonicalization fallback, correctness, error handling, incorrect comments, doc mismatches, separation of concerns, architectural simplification, naming, code duplication, minor cleanup, test boilerplate, NIT test coverage gaps

Groups reordered and renumbered by impact (2026-03-24). NON-CRITICAL first, then NIT by category: correctness > error handling > docs > architecture > naming > simplification > testing.

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
