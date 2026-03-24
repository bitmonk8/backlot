# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

New audit completed. 97 active findings across 21 groups in `docs/ISSUES.md` (10 false positives removed).

- **MUST FIX (0)**
- **NON-CRITICAL (41):** Groups 3–4, 7–12, 14–15 — silent cleanup failures, missing test coverage, weak assertions, seccomp/fork error handling, duplication, separation of concerns, placement
- **NIT (56):** Groups 1–2, 5–6, 13, 16–21 — errno style consistency, incorrect comments, TOCTOU (mitigated), canonicalization fallback, simplification, naming, test boilerplate, doc mismatches, architectural cleanup

Groups ordered by impact in ISSUES.md (NON-CRITICAL first, then NIT).

### Review notes (2026-03-24)
- **Group 1 downgraded NIT:** The "Rust 2024 unsafe soundness" claim is wrong. `macro_rules!` textual substitution places the errno dereference inside the macro's `unsafe` block. Code compiles cleanly. Finding is a style inconsistency, not a correctness issue.
- **Group 2 items 3–5 downgraded NIT:** Wrong comments, not wrong behavior. No security impact.
- **Group 3 item #8 removed (false positive):** `kill_and_reap` returns `()`, not `Result`. No error to propagate. The `Ok(())` exists for API consistency.
- **Group 3 item #7 downgraded NIT:** Inside `Drop` impl — cannot propagate errors. Deliberate design.
- **Group 4 item #13 downgraded NIT:** `connect`/`bind`/`sendto` share the same conditional block as `socket`. Existing deny test covers the code path.
- **Group 5 item #18 downgraded NIT:** TOCTOU race exists but is operationally harmless — `setup_mount_namespace` runs after `unshare(CLONE_NEWNS)`, so mount operations are namespace-private.
- **Group 6 item #19 downgraded NIT:** `is_strict_parent_of` fallback is harmless; all callers pass pre-canonicalized paths from `policy.rs` validation.
- **Group 6 item #20 removed (false positive):** Progressive prefix fallback is the function's stated algorithm, not error swallowing. Tries `/a/b/c` → `/a/b` → `/a` → `/` by design.
- **Group 6 item #21 removed (false positive):** Five canonicalization functions (not four) across four files, each serving a distinct purpose: permissive (builder), strict (validation), partial (ancestry), and two batch wrappers. Not redundant.

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
