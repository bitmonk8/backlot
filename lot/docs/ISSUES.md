# Issues

Grouped by co-fixability, ordered by impact (descending).

---

## Group 1: Critical Testing — Test Trustworthiness ✓ RESOLVED

All 8 issues fixed. Changes: removed `LOT_REQUIRE_SANDBOX`, `must_spawn` panics unconditionally, `require_cgroups` panics, `memory_hog_command`/`network_connect_command` use python3 directly with existence check, `probe_linux` asserts cross-platform fields, `kill_by_pid_self_does_not_kill` tautological assertion removed, `test_double_wait_behavior` made cross-platform, appcontainer/prerequisites silent skips replaced with `assert!`/`#[ignore]`.

---

## Group 2: Correctness & Error Handling — Sandbox Safety Bugs

Actual bugs that affect sandbox safety guarantees. These can silently weaken or break the sandbox.

### 2.1 [Correctness] Unchecked `waitpid` in Linux helper process
- **File:** lot/src/linux/mod.rs (lines 459-461)
- **Severity:** High
- **Description:** If `waitpid` returns -1 (e.g., EINTR), `inner_status` remains 0 and helper exits with 0, masking the inner child's actual exit status.

### 2.2 [Correctness] Sentinel deleted on ACL restore failure
- **File:** lot/src/windows/sentinel.rs (lines 214-232)
- **Severity:** High
- **Description:** `restore_acls_and_delete_sentinel` unconditionally deletes sentinel even when ACL restoration fails. Transient restore failure causes permanent SDDL data loss. DESIGN.md implies sentinel must survive restore failure for `cleanup_stale()` recovery. The unit test `restore_acls_and_delete_sentinel_deletes_file` encodes the buggy behavior as expected. Fix requires: (1) clarify design, (2) skip deletion on restore failure, (3) fix the test.

### 2.3 [Error-Handling] `canonicalize` silently drops errors in policy builder
- **File:** lot/src/policy_builder.rs (lines 60, 77, 93, 109)
- **Severity:** High
- **Description:** `canonicalize` silently drops all I/O errors. Permission denied silently omits path from policy — security-relevant.

### 2.4 [Error-Handling] `prctl(PR_SET_PDEATHSIG)` return discarded
- **File:** lot/src/linux/mod.rs (line 368)
- **Severity:** High
- **Description:** Return value discarded. Silently losing orphan-prevention safety net.

### 2.5 [Error-Handling] `cleanup_stale` deletes profile after ACL restore failure
- **File:** lot/src/windows/mod.rs (lines 74-80)
- **Severity:** High
- **Description:** `cleanup_stale` calls `delete_profile` even when ACL restoration fails. May leave unrecoverable stale ACLs.

---

## Group 3: High Testing — Core Untested Code Paths

Critical code with zero test coverage. These are the highest-risk untested functions.

### 3.1 [Testing] `grant_traverse` zero test coverage
- **File:** lot/src/windows/traverse_acl.rs (lines 92-577)
- **Severity:** High
- **Description:** `grant_traverse` has zero test coverage (~15 failure branches). `has_traverse_ace` never verifies return values. Most complex Windows ACL function untested.

### 3.2 [Testing] Unix lifecycle functions untested
- **File:** lot/src/unix.rs (lines 398-823)
- **Severity:** High
- **Description:** Core lifecycle functions (`check_child_error_pipe`, `kill_by_pid_guard`, `UnixSandboxedChild` methods) have no unit tests. Helper functions do have tests. The untested functions are the highest-risk code in the file.

### 3.3 [Testing] sddl.rs entirely untested
- **File:** lot/src/windows/sddl.rs (lines 1-167)
- **Severity:** High
- **Description:** Entire file (3 functions, unsafe FFI) has zero tests. Error paths uncovered.

### 3.4 [Testing] Linux namespace orchestrators untested
- **File:** lot/src/linux/namespace.rs (lines 21-511)
- **Severity:** High
- **Description:** High-level orchestrators (`setup_user_namespace`, `setup_mount_namespace`, `pivot_root`/`do_pivot_root`) have zero coverage. Helper functions do have tests. The untested orchestrators are the critical path.

---

## Group 4: Design vs. Implementation Mismatch

Documentation that does not match the code, or code that does not match the documentation.

### 4.1 [Doc-Mismatch] Job Objects UI restrictions claimed but not implemented
- **File:** docs/DESIGN.md (line 170)
- **Severity:** High
- **Description:** Job Objects section claims UI restrictions (clipboard, desktop, display settings) are applied, but `job.rs` does not implement `JOB_OBJECT_LIMIT_DESKTOP_INTERACTIONS` or equivalent UI restriction flags.

### 4.2 [Doc-Mismatch] Windows prerequisite functions undocumented public API
- **Files:** README.md, lot/src/lib.rs, lot/src/windows/prerequisites.rs
- **Severity:** Medium
- **Description:** Non-`_for_policy` Windows prerequisite functions are undocumented public API.

### 4.3 [Doc-Mismatch] Builder doc omits dedup and collapse behavior
- **File:** lot/src/policy_builder.rs (lines 9-10, 95)
- **Severity:** Low
- **Description:** Doc omits reverse overlap deduction and intra-set collapse.

### 4.4 [Doc-Mismatch] path_util `# Errors` doc overstates
- **File:** lot/src/path_util.rs (lines 38-40)
- **Severity:** Low
- **Description:** `# Errors` doc overstates when errors are returned.

---

## Group 5: Windows Handle & Input Validation

Windows-specific correctness issues around handle lifecycle and input validation.

### 5.1 [Correctness] `GetStdHandle` return unchecked
- **File:** lot/src/windows/pipe.rs (lines 95, 117)
- **Severity:** Medium
- **Description:** `GetStdHandle` return not checked for `INVALID_HANDLE_VALUE` or NULL. Invalid handle silently passed as child stdio.

### 5.2 [Correctness] `PipeHandles`/`StdioPipes` leak handles on early return
- **File:** lot/src/windows/pipe.rs (lines 19-22, 144-151)
- **Severity:** Medium
- **Description:** `PipeHandles` and `StdioPipes` store raw HANDLE with no Drop. Early return/panic leaks handles.

### 5.3 [Correctness] `build_env_block` NUL truncation
- **File:** lot/src/windows/cmdline.rs (lines 8-17)
- **Severity:** Medium
- **Description:** `build_env_block` does not validate for embedded NUL in keys/values. NUL silently truncates env block.

### 5.4 [Correctness] Drop guards missing null check
- **File:** lot/src/windows/appcontainer.rs (lines 462-466)
- **Severity:** Low
- **Description:** Drop guards check `!= INVALID_HANDLE_VALUE` only. Should also check `!is_null()`. Currently benign.

---

## Group 6: Error Handling — Silent Failures in Utilities

Functions that silently discard errors, masking failures.

### 6.1 [Error-Handling] path_util silently discards I/O and validation errors
- **File:** lot/src/path_util.rs (lines 45, 66)
- **Severity:** Medium
- **Description:** Silently discards I/O errors and `InvalidPolicy` for relative/escaping paths.

### 6.2 [Error-Handling] sddl.rs `LocalFree` return discarded, lossy conversion
- **File:** lot/src/windows/sddl.rs (lines 48-159)
- **Severity:** Medium
- **Description:** `LocalFree` return discarded in 4 locations. `from_utf16_lossy` masks corruption.

### 6.3 [Error-Handling] `JoinError` converted to string instead of propagating panic
- **File:** lot/src/lib.rs (lines 446-448)
- **Severity:** Low
- **Description:** Success branch converts `JoinError` (thread panic) to `io::Error` string. Timeout branch correctly calls `resume_unwind`. Asymmetry means panics silently become error strings in the non-timeout path.

### 6.4 [Error-Handling] Sentinel scan silently skips unreadable entries
- **File:** lot/src/windows/sentinel.rs (lines 255, 282-287)
- **Severity:** Low
- **Description:** `find_stale_sentinels_in` silently skips unreadable entries. Persistent I/O failures invisible.

---

## Group 7: Code Placement

Code located in the wrong module or crate.

### 7.1 [Placement] Tokio integration tests in library crate root
- **File:** lot/src/lib.rs (lines 550-717)
- **Severity:** High
- **Description:** `tokio_tests` integration tests in library crate root. Belongs in `tests/`.

### 7.2 [Placement] `compute_ancestors` misplaced in traverse_acl.rs
- **File:** lot/src/windows/traverse_acl.rs (lines 60-88)
- **Severity:** Medium
- **Description:** `compute_ancestors` is a pure path utility. Belongs in `path_util.rs`.

### 7.3 [Placement] `effective_env` and `DEFAULT_UNIX_PATH` misplaced
- **File:** lot/src/env_check.rs (lines 133-170)
- **Severity:** Low
- **Description:** `effective_env` pub with no external callers. `DEFAULT_UNIX_PATH` misplaced.

---

## Group 8: Correctness — Path Handling

Path comparison and canonicalization inconsistencies.

### 8.1 [Correctness] `is_strict_parent_of` lexical-only comparison
- **File:** lot/src/path_util.rs (lines 29-31)
- **Severity:** Medium
- **Description:** `is_strict_parent_of` does purely lexical comparison without canonicalization. Inconsistent with sibling `is_descendant_or_equal`.

### 8.2 [Correctness] Builder path methods asymmetric overlap handling
- **File:** lot/src/policy_builder.rs (lines 59-68, 92-101)
- **Severity:** Low
- **Description:** Builder path methods have asymmetric overlap handling. Not a bug since `build()` validates, but inconsistent.

---

## Group 9: Separation — Monolithic Functions & Mixed Concerns

Large functions mixing multiple concerns. These are refactoring targets.

### 9.1 [Separation] unix.rs: 5+ concerns in one file
- **File:** lot/src/unix.rs (lines 1-823)
- **Severity:** High
- **Description:** 5+ concerns in one file. macOS-only function in shared module.

### 9.2 [Separation] Linux `spawn` is 340-line monolith
- **File:** lot/src/linux/mod.rs (lines 148-506)
- **Severity:** Medium
- **Description:** `spawn` is 340-line monolith. `itoa_stack` is general-purpose.

### 9.3 [Separation] `spawn_with_sentinel` mixes 4 concerns
- **File:** lot/src/windows/appcontainer.rs (lines 263-811)
- **Severity:** Medium
- **Description:** `spawn_with_sentinel` mixes 4 concerns. `WindowsSandboxedChild` bundles 3 concerns.

### 9.4 [Separation] `grant_traverse` is 280-line monolith
- **File:** lot/src/windows/traverse_acl.rs (lines 159-441)
- **Severity:** Medium
- **Description:** `grant_traverse` is 280-line monolith.

### 9.5 [Separation] Three overlapping platform path definitions
- **Files:** lot/src/policy_builder.rs, platform modules, lot/src/macos/seatbelt.rs
- **Severity:** Medium
- **Description:** Three overlapping platform path definitions. Adding a path requires 3 updates.

### 9.6 [Separation] Validation logic mixed with data structure in policy.rs
- **File:** lot/src/policy.rs (lines 77-396)
- **Severity:** Medium
- **Description:** Validation logic mixed with data structure. `ResourceLimits` independent.

### 9.7 [Separation] macOS seatbelt FFI + pure-Rust generation in one module
- **File:** lot/src/macos/seatbelt.rs (lines 12-358)
- **Severity:** Low

### 9.8 [Separation] Linux namespace.rs three concerns + embedded utilities
- **File:** lot/src/linux/namespace.rs (lines 1-516)
- **Severity:** Low

### 9.9 [Separation] cgroup CgroupGuard::new mixes 3 concerns
- **File:** lot/src/linux/cgroup.rs (lines 98-283)
- **Severity:** Low

### 9.10 [Separation] macOS spawn mixes pre/child/parent
- **File:** lot/src/macos/mod.rs (lines 46-215)
- **Severity:** Low

### 9.11 [Separation] Sentinel general-purpose utility embedded, ACL ops mixed
- **File:** lot/src/windows/sentinel.rs (lines 43-232)
- **Severity:** Low

---

## Group 10: Testing — Policy, Builder & Core Library

Test coverage gaps in the policy layer and core library.

### 10.1 [Testing] policy.rs: `has_any()` untested, weak assertions
- **File:** lot/src/policy.rs (lines 162-486)
- **Severity:** Medium
- **Description:** `has_any()` untested. Tests assert only error variant. `check_deny_coverage` not tested with write/exec grants.

### 10.2 [Testing] policy_builder.rs: pure logic untested
- **File:** lot/src/policy_builder.rs (lines 39-707)
- **Severity:** Medium
- **Description:** Pure logic helpers untested. Platform convenience methods untested. Tests only check `is_err()`. Missing overlap and deny scenarios.

### 10.3 [Testing] command.rs: no builder method tests
- **File:** lot/src/command.rs (lines 42-231)
- **Severity:** Medium
- **Description:** No tests for basic builder methods. `forward_common_env` tests not hermetic.

### 10.4 [Testing] error.rs: zero unit tests
- **File:** lot/src/error.rs (lines 1-34)
- **Severity:** Medium
- **Description:** No unit tests for `SandboxError` enum. Display, From, source(), Send+Sync untested.

### 10.5 [Testing] path_util.rs: missing symlink/relative tests
- **File:** lot/src/path_util.rs (lines 14-67)
- **Severity:** Medium
- **Description:** `cfg(test)`-gated function. No symlink/relative tests. `canonicalize_best_effort` zero tests.

---

## Group 11: Testing — Linux Platform

Test coverage gaps in Linux-specific modules.

### 11.1 [Testing] linux/mod.rs: itoa, close_fds, cgroup paths untested
- **File:** lot/src/linux/mod.rs (lines 104-560)
- **Severity:** Medium
- **Description:** `itoa_stack`, `close_inherited_fds` overflow, cgroup paths, Drop-without-cleanup all untested.

### 11.2 [Testing] seccomp.rs: socket/network/ioctl filtering untested
- **File:** lot/src/linux/seccomp.rs (lines 25-355)
- **Severity:** Medium
- **Description:** No test for socket+network, ioctl filtering, or insertion order.

---

## Group 12: Testing — Windows Platform

Test coverage gaps in Windows-specific modules.

### 12.1 [Testing] windows/mod.rs: utility functions untested
- **File:** lot/src/windows/mod.rs (lines 25-111)
- **Severity:** Medium
- **Description:** `to_wide`, `platform_implicit_paths`, `cleanup_stale`, `kill_by_pid` all untested.

### 12.2 [Testing] acl_helpers.rs: error branches untested
- **File:** lot/src/windows/acl_helpers.rs (lines 224-403)
- **Severity:** Medium
- **Description:** Error branches and edge cases untested. `ELEVATION_REQUIRED_MARKER` contract unverified.

### 12.3 [Testing] job.rs: overflow/saturation untested
- **File:** lot/src/windows/job.rs (lines 61-323)
- **Severity:** Medium
- **Description:** Overflow/saturation untested. Memory limit test potentially flaky.

### 12.4 [Testing] nul_device.rs: `grant_nul_device()` zero coverage
- **File:** lot/src/windows/nul_device.rs (lines 32-46)
- **Severity:** Medium

### 12.5 [Testing] cmdline.rs: `build_env_block` zero coverage
- **File:** lot/src/windows/cmdline.rs (lines 8-18)
- **Severity:** Medium

### 12.6 [Testing] pipe.rs: `resolve_stdio_output(Piped)` untested
- **File:** lot/src/windows/pipe.rs (lines 106-126)
- **Severity:** Medium

### 12.7 [Testing] sentinel.rs: write and success-path untested
- **File:** lot/src/windows/sentinel.rs (lines 201-232)
- **Severity:** Medium
- **Description:** `write_sentinel` and success path of `restore_acls_and_delete_sentinel` untested.

### 12.8 [Testing] sentinel.rs: `is_process_alive` ACCESS_DENIED branch untested
- **File:** lot/src/windows/sentinel.rs
- **Severity:** Low
- **Description:** `is_process_alive` ACCESS_DENIED branch untested. `find_stale_sentinels_in` uses magic PID assumption.

### 12.9 [Testing] job.rs: PowerShell CLR concern
- **File:** lot/src/windows/job.rs
- **Severity:** Low
- **Description:** `memory_limit_kills_child` PowerShell CLR concern. Edge case.

---

## Group 13: Testing — macOS & Integration

Test coverage gaps in macOS and integration tests.

### 13.1 [Testing] Integration: missing network, process limit, CPU, write tests
- **File:** lot/tests/integration.rs
- **Severity:** Medium
- **Description:** No test for `allow_network(true)`, `max_processes`, `max_cpu_seconds`, write-to-allowed-path.

### 13.2 [Testing] seatbelt.rs: `apply_profile` zero coverage
- **File:** lot/src/macos/seatbelt.rs (lines 260-882)
- **Severity:** Medium
- **Description:** `apply_profile` zero coverage. `add_ancestors` guard untested. Deny-path ancestor metadata behavior undocumented by tests.

### 13.3 [Testing] lib.rs: Windows test branches bypass builder
- **File:** lot/src/lib.rs (lines 587-629)
- **Severity:** Low
- **Description:** Windows test branches bypass `SandboxPolicyBuilder`. Minor concern.

---

## Group 14: Simplification — Code Deduplication & Cleanup

Redundant code, unnecessary complexity, over-abstraction.

### 14.1 [Simplification] namespace.rs: three near-identical mount loops
- **File:** lot/src/linux/namespace.rs (lines 154-352)
- **Severity:** Medium
- **Description:** `mount_policy_paths` three near-identical loops (read/write/exec) that could share a helper. `mkdir_p` reimplements directory creation using raw `libc::mkdir` (justified for post-fork context). `create_mount_point_file` uses raw libc where safe alternative exists.

### 14.2 [Simplification] sddl.rs: manual wide string walk, missing RAII
- **File:** lot/src/windows/sddl.rs (lines 77-166)
- **Severity:** Medium
- **Description:** Manual wide string walk. Unused `dacl_defaulted`. Missing RAII guard for `sd`.

### 14.3 [Simplification] policy.rs: wrapper functions, duplicate iteration, test boilerplate
- **File:** lot/src/policy.rs (lines 162-938)
- **Severity:** Medium
- **Description:** Multiple small wrapper functions. Duplicate iteration. Repeated test boilerplate (~200 lines).

### 14.4 [Simplification] linux/mod.rs: Drop/kill overlap, duplicated test scaffolding
- **File:** lot/src/linux/mod.rs (lines 555-913)
- **Severity:** Medium
- **Description:** Drop/kill_and_cleanup overlap. Test scaffolding duplicated 4x.

### 14.5 [Simplification] acl_helpers.rs: single-call-site function, duplicated FFI
- **File:** lot/src/windows/acl_helpers.rs (lines 142-327)
- **Severity:** Medium
- **Description:** Single-call-site function. Duplicated FFI call blocks.

### 14.6 [Simplification] unix.rs: `setup_stdio_fds` and `apply_resource_limits` repeat pattern
- **File:** lot/src/unix.rs (lines 505-577)
- **Severity:** Medium
- **Description:** `setup_stdio_fds` and `apply_resource_limits` repeat same pattern 3x each.

### 14.7 [Simplification] Double validation across builder and spawn
- **Files:** lot/src/policy_builder.rs, lot/src/policy.rs, lot/src/lib.rs
- **Severity:** Medium
- **Description:** `build()` + `spawn()` both call `validate()`. Redundant canonicalization I/O.

### 14.8 [Simplification] appcontainer.rs: duplicated error-path cleanup
- **File:** lot/src/windows/appcontainer.rs (lines 110-776)
- **Severity:** Low
- **Description:** `CreateAppContainerProfile` called twice (intentional retry). Error-path cleanup duplicated across branches; guard structs would reduce duplication.

### 14.9 [Simplification] lib.rs: `kill_by_pid` dual dispatch, duplicated cfg blocks
- **File:** lot/src/lib.rs (lines 475-716)
- **Severity:** Low

### 14.10 [Simplification] env_check.rs: TEMP-var check re-implements `is_dir_accessible`
- **File:** lot/src/env_check.rs (lines 79-99)
- **Severity:** Low

### 14.11 [Simplification] path_util.rs: redundant early return
- **File:** lot/src/path_util.rs (lines 16-26)
- **Severity:** Low

### 14.12 [Simplification] seccomp.rs: scattered single-element calls, redefined constants
- **File:** lot/src/linux/seccomp.rs (lines 67-329)
- **Severity:** Low

### 14.13 [Simplification] cgroup.rs: duplicated subtree_control check
- **File:** lot/src/linux/cgroup.rs (lines 34-65)
- **Severity:** Low

### 14.14 [Simplification] macos/mod.rs: trivial wrapper with no macOS-specific cleanup
- **File:** lot/src/macos/mod.rs (lines 221-261)
- **Severity:** Low

### 14.15 [Simplification] windows/mod.rs: `to_wide`/`path_to_wide` could merge
- **File:** lot/src/windows/mod.rs (lines 25-35)
- **Severity:** Low

### 14.16 [Simplification] nul_device.rs: duplicates check already done
- **File:** lot/src/windows/nul_device.rs (lines 32-46)
- **Severity:** Low

### 14.17 [Simplification] sentinel.rs: `SYNCHRONIZE` locally redefined
- **File:** lot/src/windows/sentinel.rs (line 44)
- **Severity:** Low

### 14.18 [Simplification] sentinel.rs: extension check via Path construction
- **File:** lot/src/windows/sentinel.rs (lines 260-266)
- **Severity:** Low

---

## Group 15: Naming — Clarity Improvements

Names that do not reflect behavior or are inconsistent.

### 15.1 [Naming] `MacSandboxedChild` vs. `macos` module name
- **File:** lot/src/macos/mod.rs (line 221)
- **Severity:** Medium
- **Description:** `MacSandboxedChild` uses "Mac" while module is `macos`. Should be `MacosSandboxedChild`.

### 15.2 [Naming] `KillStyle` variants stutter type name
- **File:** lot/src/unix.rs (lines 491-498)
- **Severity:** Medium
- **Description:** `KillStyle::KillSingle`/`KillProcessGroup` stutter type name.

### 15.3 [Naming] `OverlapMode::AllowBUnderA` opaque
- **File:** lot/src/policy.rs (lines 87-95)
- **Severity:** Medium
- **Description:** `OverlapMode::AllowBUnderA` opaque without reading implementation.

### 15.4 [Naming] lib.rs: `env_check`, `platform_implicit_paths`, `&self` on `kill`/`wait`
- **File:** lot/src/lib.rs (lines 59, 149, 324-337)
- **Severity:** Low

### 15.5 [Naming] policy.rs: `canon` abbreviation, `check_deny_coverage`, `all_paths` misnomer
- **File:** lot/src/policy.rs (lines 78, 160-235)
- **Severity:** Low

### 15.6 [Naming] env_check.rs: `effective_env` suggests whole env block
- **File:** lot/src/env_check.rs (line 133)
- **Severity:** Low

### 15.7 [Naming] unix.rs: `close_parent_pipes` overstates, `kill_by_pid_guard` misleading
- **File:** lot/src/unix.rs (lines 252, 597)
- **Severity:** Low

### 15.8 [Naming] namespace.rs, cgroup.rs: `available()`, `pivot_root`/`do_pivot_root`, `has_writable_subtree`, `expected_suffix`
- **Files:** lot/src/linux/namespace.rs, lot/src/linux/cgroup.rs
- **Severity:** Low

### 15.9 [Naming] appcontainer.rs: `deny_all_file_access` understates, `_profile_name` dead field
- **File:** lot/src/windows/appcontainer.rs (lines 200, 268)
- **Severity:** Low

### 15.10 [Naming] acl_helpers.rs: `apply_dacl` generic, missing qualifiers
- **File:** lot/src/windows/acl_helpers.rs (lines 142-399)
- **Severity:** Low

### 15.11 [Naming] sddl.rs/sentinel.rs: `restore_sddl` misnomer, anonymous tuple, `write_sentinel` reads too
- **Files:** lot/src/windows/sddl.rs, lot/src/windows/sentinel.rs
- **Severity:** Low

---

## Group 16: Low Correctness — Accepted Risks

Known issues with minimal practical impact.

### 16.1 [Correctness] TOCTOU race in stale sentinel scan
- **File:** lot/src/windows/sentinel.rs (lines 267-276)
- **Severity:** Low
- **Description:** PID reuse window between check and action.

### 16.2 [Correctness] Inherent TOCTOU in sentinel design
- **File:** lot/src/windows/sentinel.rs
- **Severity:** Low
- **Description:** Documented and accepted, with small window. Listed for completeness.

