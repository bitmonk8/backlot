# Known Issues

## Windows: `compute_ancestors` silently skips non-existent paths

`compute_ancestors` calls `std::fs::canonicalize`, which fails for non-existent paths. Failed paths are silently skipped. This means `appcontainer_prerequisites_met` returns `true` vacuously when all paths fail canonicalization, and `grant_appcontainer_prerequisites` silently does nothing for those paths. Callers get no error or warning.

**Category:** Correctness
**File:** `lot/src/windows/traverse_acl.rs`

## Windows: Structural duplication between NUL device and traverse ACE functions

`apply_nul_dacl` / `apply_traverse_dacl` are structurally near-identical (build `TRUSTEE_W` + `EXPLICIT_ACCESS_W`, call `SetEntriesInAclW` + `SetNamedSecurityInfoW`, free, error-map). Same for `grant_nul_device` / `grant_traverse`. A parameterized helper would eliminate ~100 duplicated lines.

**Category:** Simplification
**Files:** `lot/src/windows/nul_device.rs`, `lot/src/windows/traverse_acl.rs`

## Windows: Two different ACE-check strategies (SDDL vs direct iteration)

NUL device check uses SDDL string matching (`sd_contains_app_packages_ace` → `sddl_has_ac_allow`). Traverse ACE check uses direct ACE iteration (`dacl_has_traverse_ace_for_app_packages` via `GetAce`/`EqualSid`). Unifying on the direct iteration approach would remove `sd_contains_app_packages_ace`, `sddl_has_ac_allow`, and ~40 lines. This also resolves the SDDL false-negative with non-empty GUID fields.

**Category:** Simplification
**Files:** `lot/src/windows/nul_device.rs`, `lot/src/windows/traverse_acl.rs`

## Windows: Tests discard return values without assertions

`nul_device_accessible_returns_bool` and `appcontainer_prerequisites_met_empty_paths` assign the result to `_`. They only verify no-panic, not correctness. Should assert expected values based on environment.

**Category:** Testing
**File:** `lot/src/windows/nul_device.rs`

## Windows: No direct tests for `has_traverse_ace` or `dacl_has_traverse_ace_for_app_packages`

These functions contain non-trivial logic (ACE iteration, SID comparison, mask check) but are only tested indirectly. The null-DACL early-return path (`dacl_has_traverse_ace_for_app_packages` returning `true`) could be tested directly.

**Category:** Testing
**File:** `lot/src/windows/traverse_acl.rs`

## Windows: No test for `grant_appcontainer_prerequisites` error path

The public entry point has no test — not even a smoke test verifying it returns `Err` when non-elevated.

**Category:** Testing
**File:** `lot/src/windows/nul_device.rs`

## Windows: `is_elevated()` re-export path

`is_elevated()` was extracted to `elevation.rs` but is still re-exported through `nul_device.rs`. The re-export should be in `mod.rs` or `lib.rs` directly. Coupled to the nul_device.rs mixed-responsibilities issue below.

**Category:** Separation of concerns
**File:** `lot/src/windows/nul_device.rs`

## Windows: No RAII wrappers for Win32 handles and allocations

Multiple functions manage `HANDLE`, `PSID`, and `PSECURITY_DESCRIPTOR` with manual `CloseHandle`/`FreeSid`/`LocalFree` calls. A `Drop`-based wrapper would eliminate leak risk on early returns.

**Category:** Simplification
**Files:** `lot/src/windows/nul_device.rs`, `lot/src/windows/traverse_acl.rs`, `lot/src/windows/appcontainer.rs`

## `SandboxPolicy::all_paths()` has no unit test

Trivial method but untested directly. Tested transitively through prerequisite functions.

**Category:** Testing
**File:** `lot/src/policy.rs`

## No test positively asserts `PrerequisitesNotMet` is produced

`spawn()` now produces `PrerequisitesNotMet` when ancestor traverse ACE grants fail, but no test asserts this error is returned for a specific scenario. Integration tests skip on this error rather than verifying it.

**Category:** Testing
**Files:** `lot/src/error.rs`, `lot/src/windows/appcontainer.rs`

## `SandboxPolicy::all_paths()` semantics

`all_paths()` now includes `deny_paths` alongside grant paths. Callers using it for ancestor traverse ACE computation (e.g., `nul_device.rs`) will grant traverse ACEs for deny path ancestors — harmless but semantically wrong. Consider splitting into `grant_paths()` for traverse/prerequisite use and `all_paths()` for cases needing both. Name `allowed_paths()` is also imprecise now that deny paths are included.

**Category:** Separation of concerns
**File:** `lot/src/policy.rs`

## Non-Windows prerequisite stubs inline in `lib.rs`

`grant_appcontainer_prerequisites_for_policy` and `appcontainer_prerequisites_met_for_policy` stubs are inline in `lib.rs` rather than following the delegation pattern used by `probe()`, `spawn()`, `cleanup_stale()`.

**Category:** Placement
**File:** `lot/src/lib.rs`

## `_for_policy` wrappers placement in `nul_device.rs`

`appcontainer_prerequisites_met_for_policy` and `grant_appcontainer_prerequisites_for_policy` couple `nul_device.rs` to `SandboxPolicy`. These orchestration wrappers belong in `appcontainer.rs` which owns the AppContainer lifecycle.

**Category:** Placement
**File:** `lot/src/windows/nul_device.rs`


## Spawn-time traverse grant misclassifies transient errors as `PrerequisitesNotMet`

`grant_traverse` can fail for transient reasons (I/O error, path deleted, file locked). The spawn code catches all errors via `.is_err()`, discards the cause, and returns `PrerequisitesNotMet`. This misclassifies transient failures as a prerequisites problem. Fix would require distinguishing permission-denied from other I/O errors inside `grant_traverse`.

**Category:** Correctness
**File:** `lot/src/windows/appcontainer.rs`

## Spawn-time traverse ACE grants are not rolled back on failure

The traverse ACE grant loop in `spawn_inner` modifies DACLs one ancestor at a time. If the Nth ancestor fails, ancestors 1..N-1 retain their new ACEs permanently. Similarly, if sentinel write or spawn fails after grants succeed, the sentinel-based rollback only covers `all_paths`, not ancestor directories. The ACEs are idempotent and harmless (traverse-only, no-inheritance), so the leaked state has no security impact.

**Category:** Correctness
**File:** `lot/src/windows/appcontainer.rs`

## Silent test skipping on `PrerequisitesNotMet` masks regressions

Both `try_spawn` helpers (in `appcontainer.rs` unit tests and `integration.rs`) return `None` on `PrerequisitesNotMet`, causing tests to silently pass without exercising sandbox logic. On a non-elevated CI runner, all Windows sandbox tests pass vacuously. No mechanism detects systematic skipping.

**Category:** Testing
**Files:** `lot/src/windows/appcontainer.rs`, `lot/tests/integration.rs`

## Spawn-time grant loop checks NUL device unconditionally

`nul_device_accessible()` runs even when `failed` is already non-empty, making the NUL check redundant in that case. Minor inefficiency — could short-circuit.

**Category:** Simplification
**File:** `lot/src/windows/appcontainer.rs`

## Two `try_spawn` helpers with same name but different semantics

`try_spawn` in `appcontainer.rs` unit tests only skips `PrerequisitesNotMet`. `try_spawn` in `integration.rs` also skips `Setup` errors and logs diagnostics. Same name, different behavior.

**Category:** Naming
**Files:** `lot/src/windows/appcontainer.rs`, `lot/tests/integration.rs`

## Windows: TOCTOU in `grant_traverse` double DACL read

`grant_traverse` calls `has_traverse_ace` (reads + frees DACL, returns bool), then reads the DACL again with `GetNamedSecurityInfoW`. Between the two calls another process could modify the DACL. Low impact (worst case: duplicate benign ACE), but also wastes a syscall round-trip. Fix: read once, check in-memory, apply if missing.

**Category:** Correctness
**File:** `lot/src/windows/traverse_acl.rs`

## Windows: `compute_ancestors` tests missing edge cases

Tests cover empty input, single path, and deduplication. Missing: root path as input, UNC paths, overlapping prefix paths, paths with trailing backslashes.

**Category:** Testing
**File:** `lot/src/windows/traverse_acl.rs`

## Windows: `nul_device.rs` mixes unrelated responsibilities

After traverse ACL extraction, `nul_device.rs` still contains NUL device logic, `allocate_app_packages_sid` (general SID allocation), `is_elevated` (general privilege query), and the public prerequisites API. Only the NUL device logic matches the module name.

**Category:** Separation of concerns
**File:** `lot/src/windows/nul_device.rs`

## Spawn-time grant loop could use iterator combinators

The imperative `for`/`if`/`push` loop collecting failed ancestors in `spawn_inner` could be a `filter`+`collect` one-liner.

**Category:** Simplification
**File:** `lot/src/windows/appcontainer.rs`

## `PrerequisitesNotMet` payload could be simplified

The structured fields `missing_paths` and `nul_device_missing` are never inspected by any match arm. A simpler `PrerequisitesNotMet(String)` with a pre-formatted message would reduce complexity.

**Category:** Simplification
**File:** `lot/src/error.rs`

## CLI: Exit code truncation on Windows

`ExitCode::from(u8::try_from(code).unwrap_or(1))` collapses Windows 32-bit exit codes (e.g. `0xC0000005` for access violation) to `1`. This is a limitation of Rust's `ExitCode` type which only accepts `u8`. Using `std::process::exit(code)` would preserve the full value but skips destructors.

**Category:** Correctness
**File:** `lot-cli/src/main.rs`

## CLI: No tests in `lot-cli` crate

`build_policy`, config deserialization, and `exit_code_from_status` are pure functions with no test coverage. The CLI has no `[dev-dependencies]` and no test infrastructure.

**Category:** Testing
**File:** `lot-cli/src/main.rs`

## CLI: Config types could be extracted to `config.rs`

`SandboxConfig` and its sub-structs plus `build_policy` form a distinct concern from CLI dispatch. Extracting to a `config.rs` module within `lot-cli/src/` would improve testability and separation of concerns. Currently manageable at ~350 lines.

**Category:** Separation of concerns
**File:** `lot-cli/src/main.rs`

## CLI: `cmd_setup` policy covers only `temp_dir`

`lot setup` builds a policy with only `write_path(temp_dir)`. If CI integration tests reference additional paths beyond temp_dir ancestors, prerequisites won't cover them. Tests that need other paths will fail loudly with `PrerequisitesNotMet`, so risk is low.

**Category:** Correctness
**File:** `lot-cli/src/main.rs`

## CI does not test `lot-cli` crate

All test jobs run `cargo test -p lot`. Any future tests added to `lot-cli` will not execute in CI. Add `cargo test -p lot-cli` step when tests are written.

**Category:** Testing
**File:** `.github/workflows/ci.yml`

## No seatbelt unit test for deny rules in generated SBPL profile

The seatbelt test module has no test that sets `deny_paths` on the policy and verifies the generated profile contains `(deny file-read* ...)`, `(deny file-write* ...)`, `(deny file-read-metadata ...)`, `(deny process-exec ...)`, and `(deny file-map-executable ...)` rules, or that they appear after allow rules.

**Category:** Testing
**File:** `lot/src/macos/seatbelt.rs`

## No integration test for deny path blocking writes

The deny path integration test only checks that reading from a denied path fails. A test with `write_paths` containing the parent and `deny_paths` containing a child, then attempting to write inside the denied subtree, would cover the write-deny path.

**Category:** Testing
**File:** `lot/tests/integration.rs`

## No builder unit tests for `deny_path()` / `deny_paths()`

The builder has no tests for deny path addition, deduplication, or silent skip of nonexistent paths.

**Category:** Testing
**File:** `lot/src/policy_builder.rs`

## No integration test for deny path blocking execution

No test attempts to execute a binary inside a denied subtree. The integration test only covers read-denial.

**Category:** Testing
**File:** `lot/tests/integration.rs`

## No unit test for `deny_access()` / `apply_ace()` deny mode

`deny_access()` delegates to `apply_ace()` with `DENY_ACCESS` mode, but no unit test exercises this path directly.

**Category:** Testing
**File:** `lot/src/windows/appcontainer.rs`

## Symlink-into-deny-path behavior untested

No test creates a symlink pointing into a denied subtree to verify path resolution behavior across platforms.

**Category:** Testing
**File:** `lot/tests/integration.rs`

## `deny_access()` naming

`deny_access` is generic. `deny_all_file_access` would be more precise since it denies `FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE`.

**Category:** Naming
**File:** `lot/src/windows/appcontainer.rs`

## `try_wait` calls `waitpid` before atomically claiming the reap

In `try_wait`, `waitpid(WNOHANG)` is called before the `compare_exchange` that marks the child as reaped. If concurrent `wait()` and `try_wait()` race, `wait()` could get `ECHILD`. In practice, `SandboxedChild` is not `Sync` and `wait`/`try_wait` take `&self`/`&mut self` making true concurrency unlikely without explicit `Arc<Mutex>` wrapping.

**Category:** Correctness
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

## `Drop` for `LinuxSandboxedChild` does not set `waited=true` after reaping

Inconsistency: `Drop` kills and reaps the helper but does not set `waited` to `true`. Benign because `Drop` takes `&mut self` (exclusive access).

**Category:** Correctness
**File:** `lot/src/linux/mod.rs`

## ~160 lines of duplicated `SandboxedChild` methods across Linux and macOS

`wait`, `try_wait`, `wait_with_output`, `take_stdin/stdout/stderr`, `close_fds`, `kill_and_cleanup`, and `Drop` are near-identical between `LinuxSandboxedChild` and `MacSandboxedChild`. A shared struct or trait in `unix.rs` could consolidate them.

**Category:** Simplification
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

## `setup_stdio_pipes` has fragile inline fd-cleanup closures

Four nearly-identical `map_err` closures manually close accumulated fds on failure. A guard-based approach would centralize cleanup and prevent omissions when adding new pipe steps.

**Category:** Simplification
**File:** `lot/src/unix.rs`

## No test for `path_to_str` non-UTF-8 rejection

`path_to_str` replaced silent-skip with explicit error on non-UTF-8 paths. No unit test verifies this behavior.

**Category:** Testing
**File:** `lot/src/linux/namespace.rs`

## No test for conditional system library and `/etc` file mounts

Conditional mounts based on `exec_paths.is_empty()` and `allow_network` are untested. Security-relevant behavioral changes.

**Category:** Testing
**File:** `lot/src/linux/namespace.rs`

## No test for `AtomicBool` double-wait prevention

`wait()` and `try_wait()` gained `compare_exchange`-based guards. No test verifies that calling `wait()` twice returns an error.

**Category:** Testing
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

## `bind_mount_file_readonly` and `mount_dev_node` duplicate file-creation-then-bind-mount pattern

Both create an empty file as a mount point using the same `open(O_CREAT | O_WRONLY)` + `close` pattern, then bind-mount. ~10 duplicated lines.

**Category:** Simplification
**File:** `lot/src/linux/namespace.rs`

## QPC uniqueness weaker than atomic counter for concurrent profile names

`unique_profile_name()` replaced `AtomicU64` with `QueryPerformanceCounter`. QPC does not guarantee distinct values for concurrent calls from different threads. The `create_profile` retry on `ERROR_ALREADY_EXISTS` mitigates this, but the race window exists. Revisit if collisions are observed.

**Category:** Correctness
**File:** `lot/src/windows/appcontainer.rs`

## `policy.rs` validate() canonicalization loop duplicated 4 times

The `for p in &self.X_paths { match canon(...) { ... } }` block is copy-pasted for read, write, exec, and deny paths. A helper function `canon_collect(paths, label, errors) -> Vec<PathBuf>` would eliminate the duplication. The `if let Err(InvalidPolicy(msg))` pattern is also repeated 8 times for overlap/coverage/limits checks.

**Category:** Simplification
**File:** `lot/src/policy.rs`

## No test for multi-error accumulation in `validate()`

`validate()` now collects all errors and joins with `"; "`. No test verifies that multiple simultaneous violations produce a combined error message.

**Category:** Testing
**File:** `lot/src/policy.rs`

## Prerequisite API remains in `nul_device.rs`

`grant_appcontainer_prerequisites`, `appcontainer_prerequisites_met`, and their `_for_policy` variants orchestrate both NUL device and traverse ACL grants. They belong in a dedicated `prerequisites.rs` module. `allocate_app_packages_sid()` is also in `nul_device.rs` but used by `traverse_acl.rs`. Coupled to the `is_elevated` re-export path issue.

**Category:** Separation of concerns
**File:** `lot/src/windows/nul_device.rs`
