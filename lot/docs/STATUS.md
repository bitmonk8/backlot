# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

New audit completed. 100 active findings across 21 groups in `docs/ISSUES.md` (7 false positives removed).

- **MUST FIX (0)**
- **NON-CRITICAL (47):** Groups 3–12, 14–15 — silent cleanup failures, missing test coverage, TOCTOU, canonicalization error handling, weak assertions, seccomp/fork error handling, duplication, separation of concerns, placement
- **NIT (53):** Groups 1–2, 13, 16–21 — errno style consistency, incorrect comments, simplification, naming, test boilerplate, doc mismatches, architectural cleanup

Groups ordered by impact in ISSUES.md (NON-CRITICAL first, then NIT).

### Review notes (2026-03-24)
- **Group 1 downgraded NIT:** The "Rust 2024 unsafe soundness" claim is wrong. `macro_rules!` textual substitution places the errno dereference inside the macro's `unsafe` block. Code compiles cleanly. Finding is a style inconsistency, not a correctness issue.
- **Group 2 items 3–5 downgraded NIT:** Wrong comments, not wrong behavior. No security impact.

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
