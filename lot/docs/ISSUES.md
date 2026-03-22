# Issues

Grouped by co-fixability, ordered by impact (descending).

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


