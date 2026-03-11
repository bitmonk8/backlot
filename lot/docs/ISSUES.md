# Known Issues

## Windows: Duplicate `GetNamedSecurityInfoW` in `grant_nul_device_access`

`grant_nul_device_access()` calls `nul_device_accessible()` (which reads the NUL device DACL), then reads the DACL again for modification. A single fetch could serve both the check and the modification.

**File:** `src/windows/nul_device.rs`

## Windows: Replace SDDL text parsing with direct ACE inspection

`sd_contains_app_packages_ace` converts the security descriptor to an SDDL string, then does string matching to find the AC ACE. This introduces a UTF-16-to-String round-trip, a manual `wcslen` loop, and fragile text parsing. Using `GetAclInformation`/`GetAce` to inspect ACEs directly would eliminate all three. This also subsumes the SDDL parser false-negative issue below.

**File:** `src/windows/nul_device.rs`

## Windows: SDDL parser false-negative with non-empty GUID fields

`sddl_has_ac_allow` matches `;;;AC)` which assumes both object_guid and inherit_object_guid fields are empty. An ACE like `(A;;FR;{guid};;AC)` would not match. In practice the ACE we write has empty GUIDs, so this only affects detecting ACEs set by other tools. Resolves automatically if SDDL parsing is replaced with direct ACE inspection.

**File:** `src/windows/nul_device.rs`

## Windows: `grant_nul_device_access` has no test coverage

The mutation path (DACL write) is untested. Needs at minimum: verify it returns an error (not panic) without elevation; verify idempotency contract when access already granted.

**File:** `src/windows/nul_device.rs`

## Windows: No RAII wrappers for Win32 handles and allocations

`grant_nul_device_access` and `can_modify_nul_device` manage `HANDLE`, `PSID`, and `PSECURITY_DESCRIPTOR` with manual `CloseHandle`/`FreeSid`/`LocalFree` calls. Adding an early return between acquisition and cleanup would leak. A `Drop`-based wrapper (e.g. `struct OwnedHandle(HANDLE)`) would eliminate this fragility.

**Files:** `src/windows/nul_device.rs`, `src/windows/appcontainer.rs`

## Windows: No integration test for NUL device API

No integration test exercises the NUL device functions end-to-end. A test that calls `nul_device_accessible()` and verifies the re-export from the crate root would catch regressions.

**File:** `src/windows/nul_device.rs`, `tests/integration.rs`
