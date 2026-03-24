# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

95 active findings across 21 groups in `docs/ISSUES.md` (19 false positives removed during triage, 1 merged duplicate).

- **MUST FIX (0)**
- **NON-CRITICAL (18):** Groups 1–5 — missing security-critical test coverage, weak assertions, lifecycle test gaps, silent cleanup failures, placement
- **NIT (77):** Groups 6–21 — TOCTOU (mitigated), canonicalization fallback, correctness, error handling, incorrect comments, doc mismatches, separation of concerns, architectural simplification, naming, code duplication, minor cleanup, test boilerplate, NIT test coverage gaps

Groups reordered and renumbered by impact (2026-03-24). NON-CRITICAL first, then NIT by category: correctness > error handling > docs > architecture > naming > simplification > testing.

### Review notes (2026-03-24)
- **Old Group 1 (now 15) downgraded NIT:** The "Rust 2024 unsafe soundness" claim is wrong. `macro_rules!` textual substitution places the errno dereference inside the macro's `unsafe` block. Code compiles cleanly. Finding is a style inconsistency, not a correctness issue.
- **Old Group 2 (now 11) items downgraded NIT:** Wrong comments, not wrong behavior. No security impact.
- **Old Group 3 (now 4) item removed (false positive):** `kill_and_reap` returns `()`, not `Result`. No error to propagate. The `Ok(())` exists for API consistency.
- **Old Group 3 (now 4) item downgraded NIT:** Inside `Drop` impl — cannot propagate errors. Deliberate design.
- **Old Group 4 (now 1) item downgraded NIT:** `connect`/`bind`/`sendto` share the same conditional block as `socket`. Existing deny test covers the code path.
- **Old Group 5 (now 6) item downgraded NIT:** TOCTOU race exists but is operationally harmless — `setup_mount_namespace` runs after `unshare(CLONE_NEWNS)`, so mount operations are namespace-private.
- **Old Group 6 (now 7) item downgraded NIT:** `is_strict_parent_of` fallback is harmless; all callers pass pre-canonicalized paths from `policy.rs` validation.
- **Old Group 6 items removed (false positives):** Progressive prefix fallback is the function's stated algorithm. Five canonicalization functions across four files each serve a distinct purpose.
- **Old Group 7 (now 3) item corrected:** `apply_resource_limits` is at lines 589-604, not 572-604. Description updated to note `set_rlimit_nofile_succeeds` tests a different resource.
- **Old Group 7 items removed (false positives):** `validate_kill_pid` is `#[cfg(feature = "tokio")]` — CI runs all tests with `--features tokio`. `wait_with_output_timeout` has integration tests in `tokio_tests` module.
- **Old Group 8 (now 2) items removed (false positives):** `probe_linux` test explicitly asserts cross-platform flags. `require_cgroups()` uses `assert!` which panics. Various items downgraded or corrected.
- **Old Group 9 (now 9) downgraded NIT:** `fork_with_seccomp` is a test helper. SIGSYS would not go unnoticed — child can't write "OK" to pipe.
- **Old Group 10 (now 17) downgraded NIT:** All three are minor code hygiene simplifications in small functions.
- **Old Group 11 (now 5) confirmed NON-CRITICAL:** Both `set_rlimit` and `apply_resource_limits` are `#[cfg(target_os = "macos")]` and only called from `macos/mod.rs`.
- **Old Group 12 (now 12) downgraded NIT:** Only `Unsupported` variant is relevant to Graceful Degradation. Doc mismatch item removed (false positive).
- **Old Group 14 (now 13) items downgraded/removed:** Mount namespace is ~200 lines (not ~500). Linux spawn fork semantics require unified control flow. Windows ACL modules have distinct responsibilities.
- **Old Group 15 (now 10) items removed/corrected:** `getrlimit(RLIMIT_NOFILE)` effectively never fails. `to_str()` always succeeds on `format!`-constructed paths. `close(fd)` finding is production code, not a test helper.

### Group 1 review (2026-03-24)
- **Item 4 downgraded NIT:** `is_apparmor_restricted` regression would not silently disable sandboxing — wrong `true` makes `probe()` report unavailable, wrong `false` causes explicit `unshare` error. Neither case is silent. Description corrected.
- **Items 6+7 merged:** Both describe the same gap (no test for `allow_network(true)`) from source vs. integration test perspective. Merged into item 6. Finding count reduced by 1.
- **Items 1, 2, 3, 5 confirmed:** Line numbers, descriptions, and severities are accurate.

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
