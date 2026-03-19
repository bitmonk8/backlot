# Known Issues

Issues grouped by code area, ordered by impact. Issues within a group touch overlapping files and benefit from co-implementation.

---

## 2. Unix: SandboxedChild Lifecycle (`linux/mod.rs`, `macos/mod.rs`, `unix.rs`)

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

## 3. Windows: AppContainer Spawn (`appcontainer.rs`)

Issues specific to the AppContainer process creation and ACL grant/deny flow. The DACL primitive extraction has landed (shared `modify_dacl` in `acl_helpers.rs`). TEST_LOCK poisoning cascades all Windows unit tests, and missing deny-mode test coverage leaves a key code path unverified.

### `create_sandboxed_process` takes 9 arguments including 6 pipe handles

The function passes child and parent pipe handles individually.

**Fix:** Bundle the 6 pipe handles into a struct.
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

### `_for_policy` prerequisite functions exclude deny paths

`appcontainer_prerequisites_met_for_policy` and `grant_appcontainer_prerequisites_for_policy` use `policy.grant_paths()` which excludes deny paths. But `spawn_inner` computes ancestors from all paths including deny paths. If a deny path has ancestors under system directories requiring elevation, the prerequisites check will report "met" while `spawn` will fail with `PrerequisitesNotMet`.

**Fix:** Include deny paths in the `_for_policy` functions' path collection.
**File:** `lot/src/windows/prerequisites.rs`

### `OwnedSecurityDescriptor` and `OwnedAcl` could share implementation

Both wrap a pointer and call `LocalFree` on drop. A single generic `LocalFreeGuard<T>` would eliminate ~30 lines of near-duplicate code.

**Fix:** Unify into a generic wrapper. Marginal benefit given the types are small.
**File:** `lot/src/windows/acl_helpers.rs`

### `dacl_has_app_packages_ace` ACE iteration loop untested with real DACLs

Only the null-DACL early-return path is tested. The entire ACE iteration loop (GetAclInformation, GetAce, EqualSid, mask matching) has no direct test. Round-trip testing requires elevation to grant an ACE then check it.

**Fix:** Add round-trip test behind `#[ignore]` or test with a mock DACL if feasible.
**File:** `lot/src/windows/acl_helpers.rs`

---

## 4. Integration Tests (`tests/integration.rs`)

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

## 5. CLI (`lot-cli/`)

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

## 6. Linux: Namespace Setup (`linux/namespace.rs`)

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

## 7. macOS: Seatbelt (`macos/seatbelt.rs`)

Single test gap. Deny paths work in integration tests but the SBPL generation is not unit-tested for deny rule ordering.

### Tests: No seatbelt unit test for deny rules in generated SBPL profile

No test sets `deny_paths` on the policy and verifies the generated profile contains deny rules appearing after allow rules.

**Fix:** Add test with deny_paths, assert profile contains `(deny file-read*`, `(deny file-write*`, etc.
**File:** `lot/src/macos/seatbelt.rs`

---

## 8. Windows: Command-line (`cmdline.rs`)

Edge-case test gap. Non-BMP characters are uncommon in process arguments; unpaired surrogates are rare but could cause panics if mishandled.

### Tests: No test for non-BMP Unicode or unpaired surrogates

Tests cover spaces, quotes, backslashes, empty args. Missing: non-BMP characters (emoji) and unpaired surrogates.

**Fix:** Add tests with non-BMP code points and unpaired surrogates via `OsString::from_wide`.
**File:** `lot/src/windows/cmdline.rs`

---

## 9. Error Types (`error.rs`)

Optional simplification. The structured fields are unused today but could be useful if callers start matching on them.

### `PrerequisitesNotMet` payload

The structured fields `missing_paths` and `nul_device_missing` are used in the `Display` format for diagnostics but never inspected by any match arm. A simpler `PrerequisitesNotMet(String)` with a pre-formatted message would reduce complexity at the cost of less structured diagnostic data.

**Fix:** Optional. Tradeoff between structured data and simplicity.
**File:** `lot/src/error.rs`

---

## 10. Env/Path Module Structure (`env_check.rs`, `policy.rs`)

Issues deferred from the env/path validation extraction. These are naming, placement, and test coverage refinements.

### `path_contains` naming ambiguity

`path_contains(parent, child)` could be read as substring containment. `any_path_contains` inherits the same ambiguity. `is_accessible_precanonicalized` embeds implementation detail in its name.

**Fix:** Rename to `is_descendant_or_equal`, `any_ancestor_of`, `is_dir_accessible` or similar.
**Files:** `lot/src/env_check.rs`

### Module name `env_check` covers general path utilities

`path_contains`, `normalize_lexical`, `canonicalize_existing_prefix` are general path utilities placed in `env_check.rs`. They have no inherent connection to environment variable checking.

**Fix:** Extract to a shared `path_util.rs` module. Have both `env_check.rs` and `policy.rs` import from it. `is_parent_of` in `policy.rs` could also delegate to the shared utility.
**Files:** `lot/src/env_check.rs`, `lot/src/policy.rs`

### `platform_implicit_read_paths` encodes platform knowledge in cross-platform module

Per-OS path lists gated by `#[cfg]` blocks belong in the respective platform modules.

**Fix:** Move lists to platform modules, expose via a dispatched function.
**Files:** `lot/src/env_check.rs`, `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`, `lot/src/windows/mod.rs`

### `canonicalize_existing_prefix` has no direct test

Non-trivial logic (iteratively popping path components, reversing, re-appending). The symlink-resolution behavior is untested.

**Fix:** Add test with a symlink where an ancestor resolves differently. Add test for full-fallback branch (no ancestor exists).
**File:** `lot/src/env_check.rs`

### `is_accessible_precanonicalized` has no direct test

Tested indirectly through integration. A unit test with pre-canonicalized arrays would isolate the logic.

**File:** `lot/src/env_check.rs`

### No test for `path_contains` with actual symlinks

The doc comment mentions `/var` → `/private/var` on macOS but no test exercises this.

**Fix:** Add macOS-specific test using `/var/tmp`.
**File:** `lot/src/env_check.rs`

### Reverse partial canonicalization (parent fails, child succeeds) untested

When `canon_parent` fails but `canon_child` succeeds, the fallback logic is untested.

**File:** `lot/src/env_check.rs`

### `kill_by_pid` on all platforms has no tests

The guard logic (rejecting PID 0, preventing self-kill) is untested. Best-effort functions, but guard correctness matters.

**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`, `lot/src/windows/mod.rs`
