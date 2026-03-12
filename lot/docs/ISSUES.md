# Known Issues

## Windows: `compute_ancestors` silently skips non-existent paths

`compute_ancestors` calls `std::fs::canonicalize`, which fails for non-existent paths. Failed paths are silently skipped. This means `appcontainer_prerequisites_met` returns `true` vacuously when all paths fail canonicalization, and `grant_appcontainer_prerequisites` silently does nothing for those paths. Callers get no error or warning.

**Category:** Correctness
**File:** `src/windows/nul_device.rs`

## Windows: Structural duplication between NUL device and traverse ACE functions

`apply_nul_dacl` / `apply_traverse_dacl` are structurally near-identical (build `TRUSTEE_W` + `EXPLICIT_ACCESS_W`, call `SetEntriesInAclW` + `SetNamedSecurityInfoW`, free, error-map). Same for `grant_nul_device` / `grant_traverse`. A parameterized helper would eliminate ~100 duplicated lines.

**Category:** Simplification
**File:** `src/windows/nul_device.rs`

## Windows: Two different ACE-check strategies (SDDL vs direct iteration)

NUL device check uses SDDL string matching (`sd_contains_app_packages_ace` → `sddl_has_ac_allow`). Traverse ACE check uses direct ACE iteration (`dacl_has_traverse_ace_for_app_packages` via `GetAce`/`EqualSid`). Unifying on the direct iteration approach would remove `sd_contains_app_packages_ace`, `sddl_has_ac_allow`, and ~40 lines. This also resolves the SDDL false-negative with non-empty GUID fields.

**Category:** Simplification
**File:** `src/windows/nul_device.rs`

## Windows: `to_wide` / `path_to_wide` duplication

Two functions that do null-terminated UTF-16 encoding for `&str` vs `&Path`. `to_wide` is only called with the `NUL_DEVICE` constant and could be replaced by `path_to_wide(Path::new(NUL_DEVICE))`.

**Category:** Simplification
**File:** `src/windows/nul_device.rs`

## Windows: Tests discard return values without assertions

`nul_device_accessible_returns_bool` and `appcontainer_prerequisites_met_empty_paths` assign the result to `_`. They only verify no-panic, not correctness. Should assert expected values based on environment.

**Category:** Testing
**File:** `src/windows/nul_device.rs`

## Windows: No direct tests for `has_traverse_ace` or `dacl_has_traverse_ace_for_app_packages`

These functions contain non-trivial logic (ACE iteration, SID comparison, mask check) but are only tested indirectly. The null-DACL early-return path (`dacl_has_traverse_ace_for_app_packages` returning `true`) could be tested directly.

**Category:** Testing
**File:** `src/windows/nul_device.rs`

## Windows: No test for `grant_appcontainer_prerequisites` error path

The public entry point has no test — not even a smoke test verifying it returns `Err` when non-elevated.

**Category:** Testing
**File:** `src/windows/nul_device.rs`

## Windows: Module name `nul_device.rs` no longer reflects scope

More than half the file is ancestor traverse ACE logic and the unified public API. A name like `appcontainer_setup.rs` or `prerequisites.rs` would better reflect the module's scope.

**Category:** Naming / Separation of concerns
**File:** `src/windows/nul_device.rs`

## Windows: `is_elevated()` and `compute_ancestors()` placement

`is_elevated()` is a general process-privilege query with no NUL-device dependency. `compute_ancestors()` is pure path logic with no Win32 security dependency. Both could live in more appropriate modules.

**Category:** Separation of concerns
**File:** `src/windows/nul_device.rs`

## Windows: No RAII wrappers for Win32 handles and allocations

Multiple functions manage `HANDLE`, `PSID`, and `PSECURITY_DESCRIPTOR` with manual `CloseHandle`/`FreeSid`/`LocalFree` calls. A `Drop`-based wrapper would eliminate leak risk on early returns.

**Category:** Simplification
**Files:** `src/windows/nul_device.rs`, `src/windows/appcontainer.rs`
