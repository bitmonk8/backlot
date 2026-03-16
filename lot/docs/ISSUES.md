# Known Issues

## Windows: `compute_ancestors` silently skips non-existent paths

`compute_ancestors` calls `std::fs::canonicalize`, which fails for non-existent paths. Failed paths are silently skipped. This means `appcontainer_prerequisites_met` returns `true` vacuously when all paths fail canonicalization, and `grant_appcontainer_prerequisites` silently does nothing for those paths. Callers get no error or warning.

**Category:** Correctness
**File:** `src/windows/traverse_acl.rs`

## Windows: Structural duplication between NUL device and traverse ACE functions

`apply_nul_dacl` / `apply_traverse_dacl` are structurally near-identical (build `TRUSTEE_W` + `EXPLICIT_ACCESS_W`, call `SetEntriesInAclW` + `SetNamedSecurityInfoW`, free, error-map). Same for `grant_nul_device` / `grant_traverse`. A parameterized helper would eliminate ~100 duplicated lines.

**Category:** Simplification
**Files:** `src/windows/nul_device.rs`, `src/windows/traverse_acl.rs`

## Windows: Two different ACE-check strategies (SDDL vs direct iteration)

NUL device check uses SDDL string matching (`sd_contains_app_packages_ace` → `sddl_has_ac_allow`). Traverse ACE check uses direct ACE iteration (`dacl_has_traverse_ace_for_app_packages` via `GetAce`/`EqualSid`). Unifying on the direct iteration approach would remove `sd_contains_app_packages_ace`, `sddl_has_ac_allow`, and ~40 lines. This also resolves the SDDL false-negative with non-empty GUID fields.

**Category:** Simplification
**Files:** `src/windows/nul_device.rs`, `src/windows/traverse_acl.rs`

## Windows: Tests discard return values without assertions

`nul_device_accessible_returns_bool` and `appcontainer_prerequisites_met_empty_paths` assign the result to `_`. They only verify no-panic, not correctness. Should assert expected values based on environment.

**Category:** Testing
**File:** `src/windows/nul_device.rs`

## Windows: No direct tests for `has_traverse_ace` or `dacl_has_traverse_ace_for_app_packages`

These functions contain non-trivial logic (ACE iteration, SID comparison, mask check) but are only tested indirectly. The null-DACL early-return path (`dacl_has_traverse_ace_for_app_packages` returning `true`) could be tested directly.

**Category:** Testing
**File:** `src/windows/traverse_acl.rs`

## Windows: No test for `grant_appcontainer_prerequisites` error path

The public entry point has no test — not even a smoke test verifying it returns `Err` when non-elevated.

**Category:** Testing
**File:** `src/windows/nul_device.rs`

## Windows: `is_elevated()` placement

`is_elevated()` is a general process-privilege query with no NUL-device dependency. Could live in a more appropriate module.

**Category:** Separation of concerns
**File:** `src/windows/nul_device.rs`

## Windows: No RAII wrappers for Win32 handles and allocations

Multiple functions manage `HANDLE`, `PSID`, and `PSECURITY_DESCRIPTOR` with manual `CloseHandle`/`FreeSid`/`LocalFree` calls. A `Drop`-based wrapper would eliminate leak risk on early returns.

**Category:** Simplification
**Files:** `src/windows/nul_device.rs`, `src/windows/traverse_acl.rs`, `src/windows/appcontainer.rs`

## `SandboxPolicy::all_paths()` has no unit test

Trivial method but untested directly. Tested transitively through prerequisite functions.

**Category:** Testing
**File:** `src/policy.rs`

## No test positively asserts `PrerequisitesNotMet` is produced

`spawn()` now produces `PrerequisitesNotMet` when ancestor traverse ACE grants fail, but no test asserts this error is returned for a specific scenario. Integration tests skip on this error rather than verifying it.

**Category:** Testing
**Files:** `src/error.rs`, `src/windows/appcontainer.rs`

## `SandboxPolicy::all_paths()` naming

`all_paths()` could become incomplete if new path categories are added. `allowed_paths()` would be more precise.

**Category:** Naming
**File:** `src/policy.rs`

## Non-Windows prerequisite stubs inline in `lib.rs`

`grant_appcontainer_prerequisites_for_policy` and `appcontainer_prerequisites_met_for_policy` stubs are inline in `lib.rs` rather than following the delegation pattern used by `probe()`, `spawn()`, `cleanup_stale()`.

**Category:** Placement
**File:** `src/lib.rs`

## `_for_policy` wrappers placement in `nul_device.rs`

`appcontainer_prerequisites_met_for_policy` and `grant_appcontainer_prerequisites_for_policy` couple `nul_device.rs` to `SandboxPolicy`. These orchestration wrappers belong in `appcontainer.rs` which owns the AppContainer lifecycle.

**Category:** Placement
**File:** `src/windows/nul_device.rs`


## Spawn-time traverse grant misclassifies transient errors as `PrerequisitesNotMet`

`grant_traverse` can fail for transient reasons (I/O error, path deleted, file locked). The spawn code catches all errors via `.is_err()`, discards the cause, and returns `PrerequisitesNotMet`. This misclassifies transient failures as a prerequisites problem. Fix would require distinguishing permission-denied from other I/O errors inside `grant_traverse`.

**Category:** Correctness
**File:** `src/windows/appcontainer.rs`

## Spawn-time traverse ACE grants are not rolled back on failure

The traverse ACE grant loop in `spawn_inner` modifies DACLs one ancestor at a time. If the Nth ancestor fails, ancestors 1..N-1 retain their new ACEs permanently. Similarly, if sentinel write or spawn fails after grants succeed, the sentinel-based rollback only covers `all_paths`, not ancestor directories. The ACEs are idempotent and harmless (traverse-only, no-inheritance), so the leaked state has no security impact.

**Category:** Correctness
**File:** `src/windows/appcontainer.rs`

## Silent test skipping on `PrerequisitesNotMet` masks regressions

Both `try_spawn` helpers (in `appcontainer.rs` unit tests and `integration.rs`) return `None` on `PrerequisitesNotMet`, causing tests to silently pass without exercising sandbox logic. On a non-elevated CI runner, all Windows sandbox tests pass vacuously. No mechanism detects systematic skipping.

**Category:** Testing
**Files:** `src/windows/appcontainer.rs`, `tests/integration.rs`

## Spawn-time grant loop checks NUL device unconditionally

`nul_device_accessible()` runs even when `failed` is already non-empty, making the NUL check redundant in that case. Minor inefficiency — could short-circuit.

**Category:** Simplification
**File:** `src/windows/appcontainer.rs`

## Two `try_spawn` helpers with same name but different semantics

`try_spawn` in `appcontainer.rs` unit tests only skips `PrerequisitesNotMet`. `try_spawn` in `integration.rs` also skips `Setup` errors and logs diagnostics. Same name, different behavior.

**Category:** Naming
**Files:** `src/windows/appcontainer.rs`, `tests/integration.rs`

## Windows: TOCTOU in `grant_traverse` double DACL read

`grant_traverse` calls `has_traverse_ace` (reads + frees DACL, returns bool), then reads the DACL again with `GetNamedSecurityInfoW`. Between the two calls another process could modify the DACL. Low impact (worst case: duplicate benign ACE), but also wastes a syscall round-trip. Fix: read once, check in-memory, apply if missing.

**Category:** Correctness
**File:** `src/windows/traverse_acl.rs`

## Windows: `compute_ancestors` tests missing edge cases

Tests cover empty input, single path, and deduplication. Missing: root path as input, UNC paths, overlapping prefix paths, paths with trailing backslashes.

**Category:** Testing
**File:** `src/windows/traverse_acl.rs`

## Windows: `nul_device.rs` mixes unrelated responsibilities

After traverse ACL extraction, `nul_device.rs` still contains NUL device logic, `allocate_app_packages_sid` (general SID allocation), `is_elevated` (general privilege query), and the public prerequisites API. Only the NUL device logic matches the module name.

**Category:** Separation of concerns
**File:** `src/windows/nul_device.rs`

## Spawn-time grant loop could use iterator combinators

The imperative `for`/`if`/`push` loop collecting failed ancestors in `spawn_inner` could be a `filter`+`collect` one-liner.

**Category:** Simplification
**File:** `src/windows/appcontainer.rs`

## `PrerequisitesNotMet` payload could be simplified

The structured fields `missing_paths` and `nul_device_missing` are never inspected by any match arm. A simpler `PrerequisitesNotMet(String)` with a pre-formatted message would reduce complexity.

**Category:** Simplification
**File:** `src/error.rs`
