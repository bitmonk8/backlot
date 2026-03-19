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

## Spawn-time grant loop checks NUL device unconditionally

`nul_device_accessible()` runs even when `failed` is already non-empty, making the NUL check redundant in that case. Minor inefficiency — could short-circuit.

**Category:** Simplification
**File:** `lot/src/windows/appcontainer.rs`

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

## No builder unit tests for `deny_path()` / `deny_paths()`

The builder has no tests for deny path addition, deduplication, or silent skip of nonexistent paths.

**Category:** Testing
**File:** `lot/src/policy_builder.rs`

## No unit test for `deny_access()` / `apply_ace()` deny mode

`deny_access()` delegates to `apply_ace()` with `DENY_ACCESS` mode, but no unit test exercises this path directly.

**Category:** Testing
**File:** `lot/src/windows/appcontainer.rs`

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

`unique_profile_name()` uses both an `AtomicU64` counter and `QueryPerformanceCounter` for uniqueness. QPC alone does not guarantee distinct values for concurrent calls from different threads, but the atomic counter provides monotonic sequencing. The `create_profile` retry on `ERROR_ALREADY_EXISTS` further mitigates collisions. Revisit if collisions are observed.

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

## Linux: `test_probe_clone_newuser_returns_result` assumes Ok unconditionally

Test asserts `result.is_ok()` with message "probe should not return Err on a normal system". Could fail in restricted containers or seccomp-filtered environments where `fork()` is disallowed. Consider allowing `Err` as valid or gating the assertion.

**Category:** Testing
**File:** `lot/src/linux/namespace.rs`

## Windows: No cmdline test for non-BMP Unicode or unpaired surrogates

The cmdline.rs tests added for H6 cover spaces, quotes, backslashes, and empty args but do not cover non-BMP Unicode characters or unpaired surrogates. The underlying code (C1 fix) handles UTF-16 correctly via `encode_wide()`, but test coverage for these edge cases is missing.

**Category:** Testing
**File:** `lot/src/windows/cmdline.rs`

## Windows: Symlink-into-deny-path test silently skips without developer mode

`test_symlink_into_deny_path` on Windows returns early if symlink creation fails (requires developer mode or elevation). In standard CI without developer mode, the test provides no coverage. Consider `#[ignore]` with a reason or a more visible skip message.

**Category:** Testing
**File:** `lot/tests/integration.rs`

## Windows: `must_spawn` panics poison `TEST_LOCK`, cascading all appcontainer tests

The appcontainer unit tests use `must_spawn()` which calls `.expect()` on `spawn()`. When prerequisites are not met (e.g., `lot setup` not run as administrator), `spawn()` returns `PrerequisitesNotMet` and `must_spawn` panics. This poisons the `TEST_LOCK` mutex, causing all subsequent tests to fail with `PoisonError` — masking the real issue.

Fix: use `unwrap_or_else` on the mutex lock to recover from poisoning, or skip tests on `PrerequisitesNotMet` instead of panicking.

**Category:** Testing
**File:** `lot/src/windows/appcontainer.rs`

## Windows: `appcontainer.rs` has parallel ACL code path duplicating `acl_helpers`

`appcontainer.rs` has its own `apply_ace`/`grant_access`/`deny_access`/`protect_dacl` stack (~160 lines) that is structurally identical to `acl_helpers::apply_dacl`. These are two independent implementations of "modify a DACL entry for a SID on a filesystem path." The `appcontainer.rs` functions handle per-path grant/deny ACEs with SDDL backup for sentinel rollback, which makes them slightly different from the simpler `apply_dacl`, but the core DACL read-modify-write pattern is duplicated.

**Category:** Separation of concerns
**Files:** `lot/src/windows/appcontainer.rs`, `lot/src/windows/acl_helpers.rs`

## Windows: `create_sandboxed_process` takes 9 arguments including 6 pipe handles

The function passes child and parent pipe handles individually. A struct grouping the 3 child + 3 parent handles would reduce argument count and eliminate the `close_all_pipes` closure repeated 3 times.

**Category:** Simplification
**File:** `lot/src/windows/appcontainer.rs`

## Windows: `ELEVATION_REQUIRED_MARKER` belongs in shared location

The constant is defined in `traverse_acl.rs` but consumed by `appcontainer.rs`, and `acl_helpers.rs` produces the same "elevation required" string literal independently. Should be defined once in `acl_helpers.rs` or `windows/mod.rs`.

**Category:** Placement
**Files:** `lot/src/windows/traverse_acl.rs`, `lot/src/windows/acl_helpers.rs`

## Linux/macOS: `child_bail!` macro defined identically in both platforms

The macro has identical implementations in `linux/mod.rs` and `macos/mod.rs`. Could be defined once in `unix.rs` and shared.

**Category:** Simplification
**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`

## Unix `spawn_sleep` test may fail on some distros

The Unix `spawn_sleep` tokio test only has `/usr` as a read_path. The default PATH from `effective_env` includes `/bin`, `/sbin`, `/usr/local/bin`. On distros where these are real directories (not symlinks into `/usr`) and not present in the implicit paths list, the check could fail.

**Category:** Correctness
**File:** `lot/src/lib.rs`

## No unit tests for env coverage validation functions

`check_env_coverage`, `platform_implicit_read_paths`, `path_is_under`, `policy_covers_path` have no unit tests. Key missing cases: TEMP outside write_paths, PATH entry not covered by grants or implicit paths, empty command.env inheritance, multiple accumulated errors.

**Category:** Testing
**File:** `lot/src/lib.rs`

## `effective_env` closure not separately testable

The `effective_env` closure inside `check_env_coverage` captures `command.env` and contains platform-conditional logic. Extracting to a standalone function would improve testability.

**Category:** Testing
**File:** `lot/src/lib.rs`

## Repeated canonicalization in `path_is_under` is O(P*G)

`path_is_under` calls `fs::canonicalize` on both arguments every invocation. In `check_env_coverage`, grant/implicit paths are re-canonicalized for each PATH entry. Canonicalize once upfront and pass pre-resolved slices.

**Category:** Simplification
**File:** `lot/src/lib.rs`

## Env validation logic should be extracted from `lib.rs`

~160 lines of env-coverage validation (`check_env_coverage`, `platform_implicit_read_paths`, `policy_covers_path`, `path_is_under`) accreted in `lib.rs`. Should be a separate module (e.g., `env_check.rs`).

**Category:** Separation of concerns
**File:** `lot/src/lib.rs`

## `platform_implicit_read_paths` should delegate to platform modules

Contains per-platform `#[cfg]` blocks encoding platform-specific knowledge. Should delegate to each platform module, matching the `probe()`/`spawn()` dispatch pattern.

**Category:** Separation of concerns
**File:** `lot/src/lib.rs`

## `effective_env` duplicates platform env inheritance knowledge

The closure inside `check_env_coverage` has Windows-specific inheritance semantics and Unix-specific default PATH. This knowledge belongs closer to `SandboxCommand` or the platform spawn implementations. Specifically, the case-insensitive key matching (`eq_ignore_ascii_case` on Windows, exact on Unix) is duplicated from `SandboxCommand::forward_common_env` in `command.rs`. A shared `SandboxCommand::effective_var(key)` method would eliminate both copies.

**Category:** Separation of concerns
**File:** `lot/src/lib.rs`

## Duplicate test helpers across test files

`make_temp_dir()` and `set_sandbox_env()` are duplicated verbatim in `lib.rs` tokio_tests, `appcontainer.rs` tests, and `integration.rs`. Extract to shared test utility (e.g., `tests/common/mod.rs`).

**Category:** Simplification
**Files:** `lot/src/windows/appcontainer.rs`, `lot/tests/integration.rs`

## Unix integration tests don't exercise TEMP/TMP/TMPDIR coverage check

`set_sandbox_env` is no-op on non-Windows, so Unix integration tests never exercise `check_env_coverage` for TEMP/TMP/TMPDIR.

**Category:** Testing
**File:** `lot/tests/integration.rs`

## Duplicate exec_paths construction in test helpers

`make_policy` and `make_deny_policy` duplicate platform-conditional exec_paths construction; same pattern also inline in `test_deny_path_blocks_access_to_subtree`.

**Category:** Simplification
**File:** `lot/tests/integration.rs`

## `policy_covers_path` and `path_is_under` placement

General path utility functions unrelated to the public API facade. Better placed in the `policy` module alongside `SandboxPolicy`.

**Category:** Placement
**File:** `lot/src/lib.rs`

## `path_is_under` duplicates `is_parent_of` in `policy.rs`

`path_is_under` (lib.rs) and `is_parent_of` (policy.rs) both check path ancestry. `path_is_under` adds canonicalization + lexical fallback; `is_parent_of` uses component-wise comparison. Should be unified into a single function.

**Category:** Simplification
**Files:** `lot/src/lib.rs`, `lot/src/policy.rs`

## `path_is_under(parent, child)` parameter order confusing

Call sites read `path_is_under(g, dir)` which parses as "g is under dir" — the opposite of the actual semantics. Consider `path_contains(parent, child)` or `is_descendant_of(child, ancestor)`.

**Category:** Naming
**File:** `lot/src/lib.rs`

## `grant` variable name in `check_env_coverage` unclear

`grant` (from `policy.grant_paths()`) holds the union of read+write+exec paths but the name doesn't convey this. `all_grant_paths` would be clearer.

**Category:** Naming
**File:** `lot/src/lib.rs`

## `check_env_coverage` name imprecise

Name suggests it only checks env var coverage, but it validates env vars against sandbox policy accessibility. `validate_env_accessibility` would be more precise.

**Category:** Naming
**File:** `lot/src/lib.rs`

## `is_accessible` and `policy_covers_path` are near-duplicates

Both iterate a slice calling `path_is_under`. `is_accessible` (inner fn in `check_env_coverage`) checks two slices; `policy_covers_path` checks one. Should be unified.

**Category:** Simplification
**File:** `lot/src/lib.rs`

## `kill_by_pid` has platform implementation in facade

`kill_by_pid` contains `#[cfg(unix)]`/`#[cfg(windows)]` blocks with raw syscalls (`libc::kill`, `OpenProcess`/`TerminateProcess`) in `lib.rs`. Should delegate to platform modules like `probe()`/`spawn()`/`cleanup_stale()`.

**Category:** Separation of concerns
**File:** `lot/src/lib.rs`

## No consistency test between Unix default PATH and `platform_implicit_read_paths`

The Unix `effective_env` hardcodes a default PATH (`/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`). No test asserts all entries are covered by `platform_implicit_read_paths`. If the two lists diverge, `check_env_coverage` would reject valid empty-env configurations.

**Category:** Testing
**File:** `lot/src/lib.rs`

## `path_is_under` partial canonicalization fallback

When `canonicalize` succeeds for one path but fails for the other, `path_is_under` falls through to lexical comparison on the original (non-canonicalized) inputs. If the successful path involved symlink resolution, the lexical comparison operates on a different representation, potentially yielding incorrect containment results.

**Category:** Correctness
**File:** `lot/src/lib.rs`

## `normalize_lexical` pops past root for relative paths

`out.pop()` on `ParentDir` can silently discard `..` components for relative paths (e.g., `../../foo` normalizes to `foo`). Only safe if all inputs are absolute paths, which is not enforced. All current callers pass absolute paths.

**Category:** Correctness
**File:** `lot/src/lib.rs`

## `platform_implicit_read_paths` rebuilds Vec on every call

Builds a `Vec<PathBuf>` with existence checks on every `spawn()` call. These paths are static per platform. The existence filter adds no safety — a non-existent implicit path would never match a real PATH entry. Could use `LazyLock` or a static `&[&str]`.

**Category:** Simplification
**File:** `lot/src/lib.rs`

## TEMP/TMP/TMPDIR checked on all platforms unconditionally

`check_env_coverage` checks all three env var names on every platform, but `TEMP`/`TMP` are Windows-only and `TMPDIR` is Unix-only. Harmless but could use `cfg` to pick relevant names per platform.

**Category:** Simplification
**File:** `lot/src/lib.rs`
