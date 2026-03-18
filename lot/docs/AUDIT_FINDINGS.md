# Audit Findings (2026-03-18) — Consolidated

## Summary

| Severity | Count | Categories |
|----------|-------|------------|
| Critical | 2     | Correctness (2) |
| High     | 8     | Correctness (2), Error Handling (1), Testing (4), Doc Mismatch (1) |
| Medium   | 25    | Correctness (5), Error Handling (1), Simplification (7), Testing (6), Separation of Concerns (2), Naming (1), Doc Mismatch (3) |
| Low      | 6     | Separation of Concerns (1), Naming (3), Placement (1), Doc Mismatch (1) |
| **Total**| **41**| |

*1 finding removed as false positive (cgroup.rs fs::write uses `?` operator). 1 finding severity downgraded (close_inherited_fds doc is accurate).*

---

## Critical (2)

### C1. [Correctness] File: lot/src/windows/cmdline.rs — FIXED
- **Line(s):** 41
- **Description:** `arg.to_string_lossy()` silently replaces invalid UTF-16 sequences with U+FFFD (replacement character), corrupting non-UTF-16 arguments. Should iterate over `arg.encode_wide()` directly.
- **Impact:** Silent data corruption of command-line arguments could cause sandboxed processes to receive wrong arguments, undermining sandbox correctness.
- **Fix:** Rewrote `append_escaped_arg` to operate on UTF-16 code units via `encode_wide()` directly, preserving unpaired surrogates.

### C2. [Correctness] File: lot/src/macos/mod.rs — FIXED
- **Line(s):** 113-116
- **Description:** `setsid()` failure causes `_exit(71)` without writing to error pipe. Parent receives EOF and proceeds as if spawn succeeded, losing error context. Should use child_bail macro with a STEP_SETSID constant.
- **Impact:** Parent treats a failed spawn as successful. The sandboxed process may not exist or may run in an incorrect session, breaking isolation assumptions.
- **Fix:** Moved `child_bail!` macro and step constants before `setsid()` call. Added `STEP_SETSID = 1`. `setsid()` failure now writes error to pipe so parent correctly returns `SandboxError::Setup`.

---

## High (8)

### H1. [Correctness] File: lot/src/windows/traverse_acl.rs
- **Line(s):** 38-62
- **Description:** `compute_ancestors()` silently skips non-existent paths via `canonicalize()` failure. If all paths fail, returns empty Vec, causing `appcontainer_prerequisites_met` to return `true` vacuously.
- **Impact:** Vacuous truth on prerequisite check could cause sandbox to proceed without required traverse ACLs, weakening isolation.

### H2. [Correctness] File: lot/src/windows/appcontainer.rs
- **Line(s):** 560-586
- **Description:** Spawn-time `grant_traverse` errors misclassified as `PrerequisitesNotMet`. Transient I/O errors on user-owned directories are indistinguishable from missing elevated prerequisites. The logic block starts at line 560 (all_paths collection) through 586 (PrerequisitesNotMet return).
- **Impact:** Incorrect error classification prevents callers from distinguishing recoverable I/O errors from actual missing prerequisites.

### H3. [Error handling] File: lot/src/linux/namespace.rs
- **Line(s):** 45-67
- **Description:** `probe_clone_newuser()` returns `false` on fork/waitpid failure without distinguishing "feature unavailable" from "system error". Callers get no error context.
- **Impact:** System errors silently downgrade sandbox capability, potentially weakening isolation.

### H4. [Testing] File: lot/tests/integration.rs
- **Line(s):** (missing tests)
- **Description:** Missing integration tests: deny path blocking writes, deny path blocking execution, symlink-into-deny-path behavior. test_cleanup_after_drop Windows section has no actual assertion.
- **Impact:** Deny path enforcement is a core sandbox guarantee. Missing integration tests for this path.

### H5. [Testing] File: lot/src/linux/namespace.rs
- **Line(s):** 539-547
- **Description:** Only 1 test (`namespace_available_no_panic`). No coverage for: path_to_str non-UTF-8 rejection, conditional mount logic, mkdir_p edge cases, mount failures.
- **Impact:** Namespace setup is the core Linux isolation mechanism.

### H6. [Testing] File: lot/src/windows/cmdline.rs
- **Line(s):** (entire file)
- **Description:** No tests for `build_command_line()` or `append_escaped_arg()`. Missing edge cases: args with only quotes, mixed backslashes/quotes, empty args, Unicode beyond BMP.
- **Impact:** Command-line escaping bugs could allow argument injection in sandboxed processes.

### H7. [Testing] File: lot/src/windows/nul_device.rs
- **Line(s):** 340-359
- **Description:** Tests `nul_device_accessible_returns_bool` and `appcontainer_prerequisites_met_empty_paths` assign result to `_` without assertions. Only verify no-panic, not correctness.
- **Impact:** These functions gate sandbox prerequisites; tests that don't assert correctness provide false confidence.

### H8. [Documentation-implementation mismatch] File: README.md
- **Line(s):** 42-53, 92-100, 177-187
- **Description:** (1) Basic example uses struct literal syntax (`SandboxPolicy { read_paths: ... }`) but fields are private; example will not compile — must use `SandboxPolicy::new()` or `SandboxPolicyBuilder`. (2) API section shows `SandboxPolicy` with public fields (`pub read_paths`). (3) `SandboxError` enum listing omits `PrerequisitesNotMet` variant which exists in error.rs:31-36.
- **Impact:** Non-compiling examples prevent users from using the library.

---

## Medium (26)

### M1. [Correctness] File: lot/src/windows/traverse_acl.rs
- **Line(s):** 171-220
- **Description:** TOCTOU in `grant_traverse()`: reads DACL via `has_traverse_ace()`, then reads again via `GetNamedSecurityInfoW`. Between calls another process could modify the DACL. Low impact (worst case: duplicate benign ACE) but wastes a syscall.
- **Impact:** TOCTOU exists but impact is limited to a duplicate benign ACE.

### M2. [Correctness] File: lot/src/windows/appcontainer.rs
- **Line(s):** 69-79
- **Description:** `unique_profile_name()` uses QPC which doesn't guarantee distinct values for concurrent calls from different threads. The `create_profile` retry on `ERROR_ALREADY_EXISTS` mitigates but race window exists.
- **Impact:** Race window exists but retry logic mitigates.

### M3. [Correctness] File: lot/src/policy.rs, lot/src/windows/nul_device.rs
- **Line(s):** policy.rs:219-228, nul_device.rs:321,331
- **Description:** `all_paths()` includes deny_paths. `appcontainer_prerequisites_met_for_policy` and `grant_appcontainer_prerequisites_for_policy` call `policy.all_paths()` to compute ancestors, granting traverse ACEs for deny path ancestors. Harmless but semantically wrong — deny paths don't need ancestor traverse. (Note: nul_device.rs docstrings say "all paths referenced by a SandboxPolicy" which technically matches the behavior.)
- **Impact:** Semantically wrong and doc mismatch, but no security impact since extra traverse ACEs are benign.

### M4. [Correctness] File: lot-cli/src/main.rs
- **Line(s):** 229-231
- **Description:** Windows exit code path uses `std::process::exit(code)` which bypasses all Drop impls. If tokio runtime is active, it will be abruptly terminated without cleanup.
- **Impact:** CLI context, not library. Drop bypass matters less here since the process is exiting anyway.

### M5. [Correctness] File: .github/workflows/ci.yml
- **Line(s):** 95-101
- **Description:** Cgroup path construction loop has no safeguard against infinite loops if `sed`/`cut` produce empty `next` component, causing `p` to remain unchanged.
- **Impact:** CI-only impact; would hang a CI job, not affect production.

### M6. [Error handling] File: .github/workflows/ci.yml
- **Line(s):** 84-103
- **Description:** Cgroup and namespace setup steps use `2>/dev/null || true` pattern, silently masking all errors. Failures surface later as cryptic test errors.
- **Impact:** CI-only impact; makes debugging harder but doesn't affect production.

### M7. [Simplification] File: lot/src/windows/nul_device.rs, lot/src/windows/traverse_acl.rs
- **Line(s):** (multiple)
- **Description:** `apply_nul_dacl` / `apply_traverse_dacl` are structurally near-identical (~100 duplicated lines). Same for `grant_nul_device` / `grant_traverse`. A parameterized helper would consolidate. Also two different ACE-check strategies: SDDL string matching (nul_device.rs) vs direct ACE iteration (traverse_acl.rs).
- **Impact:** Duplication in security-sensitive ACL code increases risk of divergent fixes.

### M8. [Simplification] File: lot/src/linux/mod.rs, lot/src/macos/mod.rs, lot/src/unix.rs
- **Line(s):** (multiple)
- **Description:** ~160 lines of nearly identical `SandboxedChild` methods (`wait`, `try_wait`, `wait_with_output`, `take_stdin/stdout/stderr`, `close_fds`, `kill_and_cleanup`, `Drop`) duplicated between Linux and macOS backends. Could be unified in unix.rs with a shared trait or struct.
- **Impact:** Duplication across platform backends.

### M9. [Simplification] File: lot/src/linux/namespace.rs
- **Line(s):** 305-430
- **Description:** `bind_mount_readonly()`, `bind_mount_readwrite()`, `bind_mount_exec()` duplicate the bind-mount-then-remount pattern. Could unify with a flags parameter. Also `bind_mount_file_readonly` and `mount_dev_node` duplicate file-creation-then-bind-mount pattern.
- **Impact:** Duplication in mount helpers.

### M10. [Simplification] File: lot/src/policy.rs
- **Line(s):** 248-279
- **Description:** Canonicalization loop duplicated 4 times (read, write, exec, deny paths). A helper `canon_collect(paths, label, errors) -> Vec<PathBuf>` would eliminate the duplication. The `if let Err(InvalidPolicy(msg))` pattern is also repeated 8 times.
- **Impact:** Duplication in validation logic.

### M11. [Simplification] File: lot/src/unix.rs
- **Line(s):** 150-212
- **Description:** Four nearly identical fd-cleanup closures in `setup_stdio_pipes()` using `inspect_err`. A guard-based approach would centralize cleanup and prevent omissions.
- **Impact:** Duplication in fd cleanup.

### M12. [Simplification] File: lot/src/windows/appcontainer.rs
- **Line(s):** 534-847
- **Description:** Windows spawn has 4 intermediate functions (spawn -> spawn_inner -> spawn_with_sentinel -> create_sandboxed_process). Could consolidate spawn_inner and spawn_with_sentinel into a single function. Repetitive handle cleanup in `spawn_with_sentinel` (lines 714-796): three error paths duplicate the same close-pipes/free-SIDs/delete-attr-list pattern. Extract into a cleanup helper.
- **Impact:** Duplication in spawn error paths.

### M13. [Simplification] File: .github/workflows/ci.yml
- **Line(s):** 21-67
- **Description:** Three identical clippy jobs (Linux/macOS/Windows) differ only in `runs-on`. Could use matrix strategy to eliminate ~50 lines of duplication.
- **Impact:** CI config duplication.

### M14. [Testing] File: lot/src/unix.rs
- **Line(s):** (entire file)
- **Description:** Zero unit test coverage. `prepare_prefork()`, `setup_stdio_pipes()`, `read_two_fds()`, `make_pipe()` have no tests. Gap for poll/EINTR/pipe logic.
- **Impact:** Utility/helper functions for stdio pipe setup.

### M15. [Testing] File: lot-cli/src/main.rs
- **Line(s):** (entire file)
- **Description:** No tests in lot-cli crate. `build_policy()`, config deserialization, and `exit_code_from_status()` are untested pure functions.
- **Impact:** CLI crate, not the core library.

### M16. [Testing] File: lot/src/policy.rs
- **Line(s):** 219, 238
- **Description:** `all_paths()` has no direct unit test. No test for multi-error accumulation in `validate()` (multiple simultaneous violations producing combined error).
- **Impact:** Missing unit tests for policy helpers.

### M17. [Testing] File: lot/src/policy_builder.rs
- **Line(s):** 86-102
- **Description:** No tests for `deny_path()` deduplication, `deny_path()` silent skip of nonexistent paths, or `deny_paths()` batch method.
- **Impact:** Missing unit tests for builder edge cases.

### M18. [Testing] File: lot/src/windows/traverse_acl.rs
- **Line(s):** 66-167
- **Description:** No direct tests for `has_traverse_ace()` or `dacl_has_traverse_ace_for_app_packages()`. Missing edge cases in `compute_ancestors()` tests: root path, UNC paths, overlapping prefixes.
- **Impact:** Missing unit tests for ACL helpers.

### M19. [Testing] File: lot/src/windows/pipe.rs, lot/src/windows/elevation.rs
- **Line(s):** (entire files)
- **Description:** No unit tests for pipe creation helpers or elevation check.
- **Impact:** Missing unit tests for Windows helpers.

### M20. [Separation of concerns] File: lot/src/windows/nul_device.rs
- **Line(s):** (entire file)
- **Description:** Module mixes unrelated responsibilities: NUL device logic, `allocate_app_packages_sid()` (general SID utility), `is_elevated` re-export, prerequisites API orchestrating both NUL device and traverse ACL operations, and `_for_policy` wrappers. Only NUL device logic matches the module name. `allocate_app_packages_sid()` is a shared utility that belongs elsewhere. `_for_policy` wrappers belong in appcontainer.rs.
- **Impact:** Misplaced security utilities increase risk of incorrect usage or missed updates.

### M21. [Separation of concerns] File: lot/src/linux/namespace.rs
- **Line(s):** 88-197
- **Description:** `setup_mount_namespace()` performs too many responsibilities (88 lines): creates tmpfs root, sets propagation, mounts system libs/bins/config, policy paths, deny paths, device nodes. Could split into subsystem-specific functions.
- **Impact:** Monolithic mount setup function.

### M22. [Naming] File: lot/src/unix.rs
- **Line(s):** 150-212, 261
- **Description:** `setup_stdio_pipes()` returns a 6-tuple with positional values that are error-prone. A named struct would improve clarity. `idx_map` variable in `read_two_fds()` poorly named.
- **Impact:** 6-tuple return is genuinely error-prone and could lead to bugs.

### M23. [Documentation-implementation mismatch] File: lot/src/linux/mod.rs
- **Line(s):** 69-78
- **Description:** `close_inherited_fds()` doc states it handles kernel < 5.9 as a no-op. Implementation calls `close_range` unconditionally; on older kernels the syscall returns -ENOSYS which is silently ignored. The doc accurately describes this as a no-op, and the broader doc (line 73-74) acknowledges "the ETXTBSY race remains possible but spawn still works correctly." The behavior matches the docs, but inherited fds do leak on older kernels.
- **Impact:** Doc is consistent with behavior. The fd leak on older kernels is a known limitation, not a contradiction.

### M24. [Documentation-implementation mismatch] File: docs/DESIGN.md
- **Line(s):** 266-273
- **Description:** CI table claims Format and Build jobs run on "All" platforms; actual CI runs Format on ubuntu-latest only and Build on ubuntu-latest only. Clippy command shown without `--features tokio` flag (actual CI includes it). Test (Linux) documented as `ubuntu-24.04` but CI uses `ubuntu-latest`.
- **Impact:** Factual inaccuracies in design doc but no user-facing impact.

### M25. [Documentation-implementation mismatch] File: lot/src/windows/sentinel.rs
- **Line(s):** 208
- **Description:** Doc says "Subsequent lines: `path\tSDDL`" but doesn't mention the percent-encoding applied to path fields (tab/newline/CR/percent encoded).
- **Impact:** Incomplete internal doc could cause bugs if someone implements a parser based on it.

---

## Low (6)

### L1. [Separation of concerns] File: lot/src/lib.rs
- **Line(s):** 119-139
- **Description:** Asymmetric export patterns: Windows uses direct re-exports from nul_device.rs submodule, non-Windows defines inline stubs. Only `_for_policy` variants get stubs; base variants are Windows-only. `is_elevated()` has no non-Windows counterpart. Non-Windows `grant_appcontainer_prerequisites_for_policy` and `appcontainer_prerequisites_met_for_policy` stubs are inline rather than following the delegation pattern used by other cross-platform functions.
- **Impact:** Cosmetic asymmetry; no functional impact.

### L2. [Naming] File: lot/src/policy.rs
- **Line(s):** 128-153, 219-228
- **Description:** `check_cross_overlap_directional` parameter names `a_paths`/`b_paths` obscure the privilege direction; renaming to `lower_priv_paths`/`higher_priv_paths` would clarify. `all_paths()` includes deny_paths; name is technically correct but misleading for callers expecting only grant paths. A split into `grant_paths()` and `all_paths()` (or renaming to `all_policy_paths()`) would clarify.
- **Impact:** Naming ambiguity.

### L3. [Naming] File: lot/src/windows/mod.rs, lot/src/windows/cmdline.rs
- **Line(s):** mod.rs:22, cmdline.rs:6
- **Description:** `to_wide()` in mod.rs vs `os_to_wide()` in cmdline.rs — same concept (convert to null-terminated UTF-16), different names and signatures.
- **Impact:** Naming inconsistency.

### L4. [Naming] File: lot/src/linux/mod.rs, lot/src/macos/mod.rs
- **Line(s):** linux/mod.rs:205, macos/mod.rs:128
- **Description:** `helper_bail!` macro (Linux) vs `child_bail!` macro (macOS) — identical error-reporting protocol (write [step:i32, errno:i32] to pipe, exit), different names.
- **Impact:** Naming inconsistency.

### L5. [Placement] File: lot/src/windows/nul_device.rs, lot/src/windows/elevation.rs, lot/src/lib.rs
- **Line(s):** nul_device.rs:30-51,166, lib.rs:122
- **Description:** `allocate_app_packages_sid()` is a general utility misplaced in nul_device.rs. `is_elevated()` defined in elevation.rs, re-exported through nul_device.rs, then through lib.rs — should be re-exported directly from windows/mod.rs.
- **Impact:** Misplaced utilities.

### L6. [Documentation-implementation mismatch] File: .github/workflows/ci.yml
- **Line(s):** 153
- **Description:** test-windows job omits `--nocapture` flag present in test-linux (line 119) and test-macos (line 135). Inconsistent diagnostic output across platforms.
- **Impact:** Cosmetic inconsistency in CI output.
