# Project Status

## Current Phase

**Implementation and audit remediation complete** across all platforms (Linux, macOS, Windows). CI pipeline covers format, clippy, build, and test on all 3 platforms. Tests run in parallel (default thread count).

## Issues (2026-03-24)

New audit completed. 92 active findings across 21 groups in `docs/ISSUES.md` (15 false positives removed).

- **MUST FIX (0)**
- **NON-CRITICAL (30):** Groups 3–4, 7–8, 11, 14–15 — silent cleanup failures, missing test coverage, weak assertions, separation of concerns, placement
- **NIT (62):** Groups 1–2, 5–6, 9–10, 12–13, 16–21 — errno style consistency, incorrect comments, TOCTOU (mitigated), canonicalization fallback, test helper error handling, duplication/simplification, naming, test boilerplate, doc mismatches, architectural cleanup

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
- **Group 7 item #23 line range corrected:** `apply_resource_limits` is at lines 589-604, not 572-604. Description updated to note `set_rlimit_nofile_succeeds` tests a different resource.
- **Group 7 item #24 removed (false positive):** `validate_kill_pid` is `#[cfg(feature = "tokio")]` — tests must be feature-gated to compile. CI runs all tests with `--features tokio` (ci.yml lines 103, 121, 141). Tests are never skipped.
- **Group 7 item #25 removed (false positive):** `wait_with_output_timeout` has integration tests in `tokio_tests` module (integration.rs lines 1552-1654).
- **Group 8 item #30 removed (false positive):** `probe_linux` test explicitly asserts `!caps.seatbelt`, `!caps.appcontainer`, `!caps.job_objects`. Finding's blanket claim was wrong.
- **Group 8 item #31 downgraded NIT:** `validate()` is well-tested in policy.rs (28+ tests). Risk of spawn() not propagating it is low.
- **Group 8 item #34 removed (false positive):** `require_cgroups()` uses `assert!` which panics (fails the test), not silently passes. Finding described the opposite behavior.
- **Group 8 item #35 description corrected:** Not silent — prints `[diag] SKIPPED:` to stdout/stderr. Real issue is reporting as passed instead of skipped.
- **Group 8 item #36 description corrected:** Windows path does assert `!status.success()`. Finding scoped to Unix path only.
- **Group 8 item #39 downgraded NIT:** All three env var keys (`TEMP`, `TMP`, `TMPDIR`) share identical handling in a trivial loop.
- **Group 9 #40 downgraded NIT:** `fork_with_seccomp` is a test helper, not production seccomp code. SIGSYS would not go unnoticed — child can't write "OK" to pipe, so test assertion fails. Group description corrected to remove misleading "seccomp enforcement" framing.
- **Group 10 #42, #43, #44 downgraded NIT:** All three are minor code hygiene simplifications in small functions. Three explicit mount loops (~7 lines each) are clear; five `.map_err` calls are repetitive but trivial; `has_writable_delegation` duplication is minor in a ~30-line function.
- **Group 11 #46 confirmed NON-CRITICAL:** Both `set_rlimit` and `apply_resource_limits` are `#[cfg(target_os = "macos")]` and only called from `macos/mod.rs`. Placement issue is real.
- **Group 12 #47 downgraded NIT:** Only `Unsupported` variant is relevant to Graceful Degradation (mechanism unavailability). `Timeout` and `Io` are runtime/generic errors — correctly excluded from that table. Description corrected.
- **Group 12 #49 removed (false positive):** DESIGN.md line 13 is a terse directory-listing comment. Overlap deduction is documented in source code (policy_builder.rs lines 7-19). Not a doc mismatch.

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
