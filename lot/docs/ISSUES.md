# Known Issues

Issues grouped by code area, ordered by impact. Issues within a group touch overlapping files and benefit from co-implementation.

---

## 1. Windows: ACL Infrastructure (`acl_helpers.rs`, `traverse_acl.rs`, `nul_device.rs`)

These issues all involve the DACL read-modify-write pipeline, ACE checking, and handle/resource lifetime management. Fixing the shared primitive (extract `modify_dacl`) first unblocks the ACE-check unification and TOCTOU fix. RAII wrappers should go in early since every other change here benefits from them. This group ranks first because it contains resource leak vectors, a correctness issue (SDDL check is less precise than direct iteration), a TOCTOU race, and is foundational — the AppContainer spawn group depends on these primitives.

### No RAII wrappers for Win32 handles and allocations

Multiple functions manage `HANDLE`, `PSID`, and `PSECURITY_DESCRIPTOR` with manual `CloseHandle`/`FreeSid`/`LocalFree` calls. A `Drop`-based wrapper would eliminate leak risk on early returns.

**Fix:** Add newtype wrappers implementing `Drop` for each resource kind.
**Files:** `lot/src/windows/nul_device.rs`, `lot/src/windows/traverse_acl.rs`, `lot/src/windows/appcontainer.rs`

### Extract shared DACL read-modify-write primitive

`appcontainer.rs` has `apply_ace`/`grant_access`/`deny_all_file_access`/`protect_dacl` (~160 lines) duplicating the DACL read-modify-write pattern in `acl_helpers::apply_dacl`. The `appcontainer.rs` version supports `SET_ACCESS`, `DENY_ACCESS`, `REVOKE_ACCESS` modes plus `PROTECTED_DACL_SECURITY_INFORMATION`, while `acl_helpers` only supports `GRANT_ACCESS`.

**Fix:** Extract a low-level `modify_dacl(path, entries, flags)` function that handles the Win32 calls (`GetNamedSecurityInfo`/`SetEntriesInAcl`/`SetNamedSecurityInfo`/`LocalFree`). Both modules build their own `EXPLICIT_ACCESS_W` arrays and call it.
**Files:** `lot/src/windows/appcontainer.rs`, `lot/src/windows/acl_helpers.rs`

### Two different ACE-check strategies (SDDL vs direct iteration)

NUL device check uses SDDL string matching (`sddl_has_ac_allow`). Traverse ACE check uses direct ACE iteration (`dacl_has_traverse_ace_for_app_packages` via `GetAce`/`EqualSid`). The SDDL approach is less precise (doesn't verify specific access mask, doesn't model deny ACE ordering).

**Fix:** Unify on direct iteration. Parameterize `dacl_has_traverse_ace_for_app_packages` to accept an access mask argument, then use it for NUL device checks too. Remove `sd_contains_app_packages_ace`, `sddl_has_ac_allow`.
**Files:** `lot/src/windows/nul_device.rs`, `lot/src/windows/traverse_acl.rs`

### Residual TOCTOU in `grant_traverse` DACL handling

`grant_traverse` reads the DACL once and checks for the ACE in-place (the original double-read was fixed). However, it frees the security descriptor, then `apply_dacl` re-reads the DACL to merge the new ACE. Between the free and re-read, another process could modify the DACL. Low impact (worst case: duplicate benign ACE).

**Fix:** Pass the already-read DACL to `apply_dacl` (or a variant) instead of re-reading. Requires API change to the merge-and-set flow.
**File:** `lot/src/windows/traverse_acl.rs`

### `ELEVATION_REQUIRED_MARKER` belongs in `acl_helpers.rs`

The constant is defined in `traverse_acl.rs` but consumed by `appcontainer.rs`, and `acl_helpers.rs` produces the same `"elevation required"` string as a hardcoded literal independently.

**Fix:** Move constant to `acl_helpers.rs`, reference from both consumers.
**Files:** `lot/src/windows/traverse_acl.rs`, `lot/src/windows/acl_helpers.rs`

### Prerequisites API in `nul_device.rs`

`grant_appcontainer_prerequisites`, `appcontainer_prerequisites_met`, and `_for_policy` variants orchestrate both NUL device and traverse ACL grants. Only the NUL device logic matches the module name.

**Fix:** Move prerequisites API to a dedicated `prerequisites.rs` module. Move `_for_policy` wrappers to `appcontainer.rs` (they take `SandboxPolicy` and belong with the AppContainer lifecycle).
**File:** `lot/src/windows/nul_device.rs`

### Tests: `compute_ancestors` missing edge cases

Root path is tested. Missing: UNC paths, overlapping prefix paths, paths with trailing backslashes.

**Fix:** Add the three missing test cases. UNC path tests may need `#[ignore]` unless a UNC path is guaranteed to exist.
**File:** `lot/src/windows/traverse_acl.rs`

### Tests: `has_traverse_ace` lacks value-asserting test

A smoke test (`has_traverse_ace_system_directory`) exists but only verifies no-panic, not a specific return value.

**Fix:** Round-trip test (grant ACE, then check) would be meaningful but requires elevation. Alternatively, test the null-DACL early-return path directly.
**File:** `lot/src/windows/traverse_acl.rs`

### Tests: No test for `grant_appcontainer_prerequisites` error path

No test verifies it returns `Err` when non-elevated.

**Fix:** Call from non-elevated context, assert `is_err()`.
**File:** `lot/src/windows/nul_device.rs`

---

## 2. Env/Path Validation (`lib.rs` → `env_check.rs`, `policy.rs`)

All these issues involve the path containment/validation functions in `lib.rs`. The extraction to `env_check.rs` is the anchor — the correctness fixes, deduplication, and naming fixes are best done as part of that extraction. This group ranks second because `path_contains` canonicalization fallback is security-relevant: incorrect path containment checks could allow access to paths outside the sandbox policy. The `is_parent_of`/`path_contains` duplication across `lib.rs` and `policy.rs` risks divergent behavior in policy validation vs runtime checks.

### Extract env validation logic to `env_check.rs`

~192 lines of env/path validation (`check_env_coverage`, `platform_implicit_read_paths`, `policy_covers_path`, `path_contains`, `normalize_lexical`, `is_accessible`, `effective_env`) accreted in `lib.rs`. Unrelated to the public API facade.

**Fix:** Extract to `env_check.rs` module. Includes:
- Rename `check_env_coverage` to `validate_env_accessibility`
- Rename `path_is_under` to `path_contains`
- Extract `effective_env` closure to a standalone function (enables direct unit testing)
- Pre-canonicalize paths once upfront instead of per-iteration O(P*G) re-canonicalization
- `platform_implicit_read_paths` moves with `#[cfg]` blocks intact
**File:** `lot/src/lib.rs`

### `path_contains` partial canonicalization fallback

When `canonicalize` succeeds for one path but fails for the other, the function falls through to lexical comparison on the original (non-canonicalized) inputs. If the successful path involved symlink resolution, the lexical comparison operates on mismatched representations.

**Fix:** Use the canonicalized result when available for each path independently before lexical comparison.
**File:** `lot/src/lib.rs` (moving to `env_check.rs`)

### `normalize_lexical` accepts relative paths without enforcement

`out.pop()` on `ParentDir` can silently discard `..` components for relative paths (e.g., `../../foo` normalizes to `foo`). All current callers pass absolute paths.

**Fix:** Add `debug_assert!(path.is_absolute())`.
**File:** `lot/src/lib.rs` (moving to `env_check.rs`)

### `is_accessible` and `policy_covers_path` are near-duplicates

Both iterate a slice calling `path_contains`. `is_accessible` checks two slices (grant + implicit); `policy_covers_path` checks one.

**Fix:** Unify. `is_accessible` can call `policy_covers_path` on combined slices or accept multiple slices.
**File:** `lot/src/lib.rs` (moving to `env_check.rs`)

### `path_contains` duplicates `is_parent_of` in `policy.rs`

Different semantics: `is_parent_of` excludes equal paths, `path_contains` includes them and adds canonicalization + lexical fallback. Overlap exists.

**Fix:** Unify into one function with an `include_equal` parameter, or have `is_parent_of` call `path_contains` with a flag.
**Files:** `lot/src/lib.rs`, `lot/src/policy.rs`

### `grant` variable → `grant_paths`

In `check_env_coverage`, `grant` holds the union of read+write+exec paths but the name doesn't convey this.

**File:** `lot/src/lib.rs` (moving to `env_check.rs`)

### `kill_by_pid` has platform implementation in facade

Contains `#[cfg(unix)]`/`#[cfg(windows)]` blocks with raw syscalls in `lib.rs`.

**Fix:** Delegate to platform modules, matching the `probe()`/`spawn()` dispatch pattern.
**File:** `lot/src/lib.rs`

### Tests: No consistency test between Unix default PATH and `platform_implicit_read_paths`

The default PATH and implicit read paths are defined independently. If they diverge, `validate_env_accessibility` would reject valid empty-env configurations.

**Fix:** Add test asserting default PATH entries are a subset of `platform_implicit_read_paths`.
**File:** `lot/src/lib.rs` (moving to `env_check.rs`)

### Tests: Unix `spawn_sleep` test may fail on some distros

The test grants only `/usr` as a read_path and runs `/bin/sleep`. On distros where `/bin` is not a symlink to `/usr/bin`, the binary is not accessible.

**Fix:** Add `/bin` to read_paths.
**File:** `lot/src/lib.rs`

---

## 3. Unix: SandboxedChild Lifecycle (`linux/mod.rs`, `macos/mod.rs`, `unix.rs`)

All these issues involve the wait/kill/drop lifecycle shared between Linux and macOS. The deduplication issue is the anchor — extracting shared code to `unix.rs` naturally addresses the `child_bail!` macro, `try_wait` ordering, `Drop` consistency, and `setup_stdio_pipes` cleanup in one pass. This group ranks third because `try_wait` ordering is a correctness issue (albeit low-probability), and the ~160-line duplication means every future lifecycle change must be applied twice.

### ~160 lines of duplicated `SandboxedChild` methods across Linux and macOS

`wait`, `try_wait`, `wait_with_output`, `take_stdin/stdout/stderr`, `close_fds`, `kill_and_cleanup`, and `Drop` are near-identical between `LinuxSandboxedChild` and `MacSandboxedChild`. Differences: Linux uses `libc::kill` on helper pid; macOS uses `libc::killpg` on child pid. Linux `kill_and_cleanup` also drops `cgroup_guard`.

**Fix:** Extract shared struct or trait in `unix.rs`. The kill behavior difference can be parameterized.
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

### `child_bail!` macro defined identically in both platforms

Both write 8 bytes `[step:i32, errno:i32]` via `libc::write` then call `libc::_exit(1)`.

**Fix:** Move to `unix.rs`. Must remain async-signal-safe (no allocations).
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

### `try_wait` calls `waitpid` before atomically claiming the reap

In `try_wait`, `waitpid(WNOHANG)` is called before the `compare_exchange` that marks the child as reaped. If concurrent `wait()` and `try_wait()` race, `wait()` could get `ECHILD`. In practice, `SandboxedChild` is not `Sync` and `wait`/`try_wait` take `&self`/`&mut self` making true concurrency unlikely.

**Fix:** Move `compare_exchange` before `waitpid`.
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

### `Drop` for `LinuxSandboxedChild`/`MacSandboxedChild` does not set `waited=true` after reaping

Inconsistency: `Drop` kills and reaps but does not set `waited` to `true`. Benign because `Drop` takes `&mut self` (exclusive access).

**Fix:** Add `self.waited.store(true, Ordering::Release)` after `waitpid` in `Drop` for both platforms.
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

### `setup_stdio_pipes` fd-cleanup closures

Three `inspect_err` closures call `cleanup_stdio_fds` with progressively more fds. Already refactored to use a `cleanup_stdio_fds` helper (less fragile than original), but adding a new pipe step still requires updating closures.

**Fix:** Consider RAII guard approach. Marginal benefit given the function is stable.
**File:** `lot/src/unix.rs`

### Tests: No test for `AtomicBool` double-wait prevention

No test verifies that calling `wait()` twice returns an error.

**Fix:** Spawn, wait, wait again, assert `ErrorKind::InvalidInput`.
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

---

## 4. Windows: AppContainer Spawn (`appcontainer.rs`)

Issues specific to the AppContainer process creation and ACL grant/deny flow. The DACL primitive extraction (group 1) should land first since several fixes here depend on it. Ranks here because TEST_LOCK poisoning cascades all Windows unit tests, and missing deny-mode test coverage leaves a key code path unverified.

### `create_sandboxed_process` takes 9 arguments including 6 pipe handles

The function passes child and parent pipe handles individually.

**Fix:** Bundle the 6 pipe handles into a struct.
**File:** `lot/src/windows/appcontainer.rs`

### `deny_access()` → `deny_all_file_access()`

`deny_access` is generic. It denies `FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE`. Private function, mechanical rename.

**File:** `lot/src/windows/appcontainer.rs`

### Spawn-time grant loop checks NUL device unconditionally

`nul_device_accessible()` runs even when `prereq_failed` is already non-empty.

**Fix:** Wrap in `if prereq_failed.is_empty()`.
**File:** `lot/src/windows/appcontainer.rs`

### Spawn-time grant loop could use iterator combinators

The imperative loop collecting failed ancestors has an early `return Err(e)` branch for non-access-denied errors, preventing a simple `filter+collect`. A `try_fold` approach would work but may not improve readability.

**Fix:** Optional. Marginal benefit.
**File:** `lot/src/windows/appcontainer.rs`

### Tests: `must_spawn` panics poison `TEST_LOCK`, cascading all appcontainer tests

When `spawn()` returns `PrerequisitesNotMet`, `must_spawn` panics while holding the mutex. All subsequent tests fail with `PoisonError`.

**Fix:** Use `unwrap_or_else(|e| e.into_inner())` on mutex lock, or skip tests on `PrerequisitesNotMet` instead of panicking.
**File:** `lot/src/windows/appcontainer.rs`

### Tests: No unit test for `deny_all_file_access()` / `apply_ace()` deny mode

No test exercises the `DENY_ACCESS` code path.

**Fix:** Add a test with a deny path that overrides an inherited allow.
**File:** `lot/src/windows/appcontainer.rs`

### Tests: No test positively asserts `PrerequisitesNotMet` is produced

Integration tests skip on this error rather than verifying it.

**Fix:** Construct a policy referencing a system directory requiring elevation from a non-elevated context, assert `PrerequisitesNotMet`.
**Files:** `lot/src/error.rs`, `lot/src/windows/appcontainer.rs`

---

## 5. Integration Tests (`tests/integration.rs`)

Duplicate helpers increase maintenance cost for every new test. The symlink test providing zero coverage in CI and the missing Unix env-var coverage leave gaps in platform-specific validation.

### Duplicate test helpers across test files

`make_temp_dir()` and `set_sandbox_env()` are duplicated in `lib.rs` tests, `appcontainer.rs` tests, and `integration.rs`.

**Fix:** Extract to shared test utility module.
**Files:** `lot/src/windows/appcontainer.rs`, `lot/tests/integration.rs`

### Duplicate exec_paths construction in test helpers

`make_policy`, `make_deny_policy`, and `test_deny_path_blocks_access_to_subtree` duplicate platform-conditional exec_paths construction.

**Fix:** Extract to shared helper.
**File:** `lot/tests/integration.rs`

### Symlink-into-deny-path test silently skips without developer mode

`test_symlink_into_deny_path` returns early if symlink creation fails. In standard CI, the test provides no coverage.

**Fix:** Use `#[ignore]` with a reason, or enable developer mode in CI.
**File:** `lot/tests/integration.rs`

### Unix integration tests don't exercise TEMP/TMP/TMPDIR coverage check

`set_sandbox_env` is no-op on non-Windows, so Unix integration tests never exercise `validate_env_accessibility` for temp dir env vars.

**Fix:** Add env var coverage tests on Unix.
**File:** `lot/tests/integration.rs`

---

## 6. CLI (`lot-cli/`)

CI not testing `lot-cli` means regressions ship undetected. The config extraction is optional.

### CI does not test `lot-cli` crate

All test jobs run `cargo test -p lot`. The `lot-cli` crate tests are never executed.

**Fix:** Add `cargo test -p lot-cli` step to CI test jobs.
**File:** `.github/workflows/ci.yml`

### CLI config types could be extracted to `config.rs`

`SandboxConfig` and sub-structs plus `build_policy` (~58 lines of types) form a distinct concern from CLI dispatch. Currently manageable.

**Fix:** Optional. Extract when the file grows larger.
**File:** `lot-cli/src/main.rs`

---

## 7. Linux: Namespace Setup (`linux/namespace.rs`)

Minor duplication and a test gap. Low impact — the mount-point pattern is stable and the conditional-mount behavior is exercised indirectly by other tests.

### `bind_mount_file_readonly` and `mount_dev_node` duplicate file-creation-then-bind-mount pattern

Both create an empty file as a mount point using the same `open(O_CREAT | O_WRONLY)` + `close` pattern. The bind-mount step differs (readonly remount vs raw `MS_BIND`).

**Fix:** Extract `create_mount_point_file` helper for the open/close pattern.
**File:** `lot/src/linux/namespace.rs`

### Tests: No test for conditional system library and `/etc` file mounts

Conditional mounts based on `exec_paths.is_empty()` and `allow_network` are untested.

**Fix:** Add integration test exercising both branches.
**File:** `lot/src/linux/namespace.rs`

---

## 8. macOS: Seatbelt (`macos/seatbelt.rs`)

Single test gap. Deny paths work in integration tests but the SBPL generation is not unit-tested for deny rule ordering.

### Tests: No seatbelt unit test for deny rules in generated SBPL profile

No test sets `deny_paths` on the policy and verifies the generated profile contains deny rules appearing after allow rules.

**Fix:** Add test with deny_paths, assert profile contains `(deny file-read*`, `(deny file-write*`, etc.
**File:** `lot/src/macos/seatbelt.rs`

---

## 9. Windows: Command-line (`cmdline.rs`)

Edge-case test gap. Non-BMP characters are uncommon in process arguments; unpaired surrogates are rare but could cause panics if mishandled.

### Tests: No test for non-BMP Unicode or unpaired surrogates

Tests cover spaces, quotes, backslashes, empty args. Missing: non-BMP characters (emoji) and unpaired surrogates.

**Fix:** Add tests with non-BMP code points and unpaired surrogates via `OsString::from_wide`.
**File:** `lot/src/windows/cmdline.rs`

---

## 10. Error Types (`error.rs`)

Optional simplification. The structured fields are unused today but could be useful if callers start matching on them.

### `PrerequisitesNotMet` payload

The structured fields `missing_paths` and `nul_device_missing` are used in the `Display` format for diagnostics but never inspected by any match arm. A simpler `PrerequisitesNotMet(String)` with a pre-formatted message would reduce complexity at the cost of less structured diagnostic data.

**Fix:** Optional. Tradeoff between structured data and simplicity.
**File:** `lot/src/error.rs`
