# Issues

Generated from audit findings: 2026-03-24

69 active findings. 0 MUST FIX, 0 NON-CRITICAL, 69 NIT. Groups ordered by impact.

Review notes appended per group in STATUS.md.

---

## Group 3 — Missing test coverage: lifecycle

Lifecycle operations (stdio setup, timeout cleanup) lack tests.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 16 | Testing | lot/src/unix.rs | 512-566 | NIT | `setup_stdio_fds` has no direct test coverage. The fd-aliasing logic (`effective_fd` helper, `redirected` tracking array) is only exercised indirectly via integration tests that spawn sandboxed children. The aliasing case (same fd for stdout and stderr) has zero coverage. Difficult to unit-test: runs in a forked child, requires real fd manipulation. |
| 17 | Testing | lot/tests/integration.rs | 1488-1636 | NIT | Tokio timeout tests (`mod tokio_tests`) verify timeout fires and fast-child completes, but don't verify child process cleanup after timeout. The implementation does kill and reap, but tests don't assert it. |
| 18 | Testing | lot/tests/integration.rs | 435-499 | NIT | `test_cleanup_after_drop` uses `echo` (short-lived), so Unix assertions (process gone) likely pass because `echo` already exited, not because drop killed it. Windows only checks `cleanup_stale().is_ok()`, not process termination. A long-running child (e.g., `sleep 60`) would actually test drop-triggered kill. |

---

## Group 4 — Silent failures in kill/signal/cleanup paths

Process cleanup silently discards errors. Leaked processes go undiagnosed.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 19 | Error-Handling | lot/src/linux/mod.rs | 569 | NIT | `kill_by_pid` ignores `libc::kill` return. Cannot distinguish success from permission denied. |
| 20 | Error-Handling | lot/src/macos/mod.rs | 244-254 | NIT | `kill_by_pid` silently discards `libc::kill` return. Permission errors invisible. |

---

## Group 5 — TOCTOU in namespace mount point setup

Race condition in mount namespace setup path.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 21 | Correctness | lot/src/linux/namespace.rs | 247-263 | NIT | TOCTOU window: `/tmp/lot-newroot-{pid}` is in host `/tmp`. Between `rmdir` and `mkdir_p`+`mount_tmpfs`, another process could plant a symlink or directory. Operationally harmless: `setup_mount_namespace` runs after `unshare(CLONE_NEWNS)` (mod.rs:306, after unshare at :285), so mount operations are namespace-private. `mount_tmpfs` hides any planted content, `mkdir` does not follow symlinks. |

---

## Group 6 — Path canonicalization fallback in `is_strict_parent_of`

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 22 | Error-Handling | lot/src/path_util.rs | 33-34 | NIT | `is_strict_parent_of` falls back to uncanonicalized path on `canonicalize_existing_prefix` failure. Harmless in practice: all callers are in `policy.rs` validation, where paths have already been canonicalized by `canonicalize_collect`. The fallback handles only edge cases already caught upstream. |

---

## Group 7 — Remaining correctness NIT

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 23 | Correctness | lot/src/unix.rs | 519-526 | NIT | `effective_fd` returns first match in redirected array. Fragile if calling pattern changes, though safe with current 3-step logic. |

---

## Group 8 — Error handling in fork/child paths

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 24 | Error-Handling | lot/src/linux/seccomp.rs | 447 | NIT | Test helper `fork_with_seccomp` doesn't check `waitpid` return value or child exit status. SIGSYS would not go unnoticed — child can't write "OK" to pipe, so the test assertion fails. Real issue is unchecked `waitpid` return in test helper. |
| 25 | Error-Handling | lot/src/unix.rs | 377 | NIT | `child_bail` discards `libc::write` return. If broken pipe, parent sees EOF and concludes success. Defensible since `_exit(1)` follows. |

---

## Group 9 — Error handling in test helpers

Test helpers discard errors, producing confusing failures or false passes.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 26 | Error-Handling | lot/src/unix.rs | 1148-1156 | NIT | Test helper `fork_pipe_writer` discards write return value. Failed write produces empty pipe indistinguishable from success. |
| 27 | Error-Handling | lot/src/unix.rs | 1540-1549 | NIT | Test child branch discards `libc::write` return for stdout/stderr. Failures surface as confusing assertion. |
| 28 | Error-Handling | lot/src/linux/mod.rs | 792-794 | NIT | `waitpid` return value unchecked in 4 test functions. Status remains 0 on failure. |
| 29 | Error-Handling | lot/src/linux/namespace.rs | 399 | NIT | `create_mount_point_file` does not check `libc::close(fd)` return value. Production code, not a test helper. |

---

## Group 10 — Incorrect comments

Wrong descriptions of sandbox rule evaluation and environment behavior.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 30 | Doc-Mismatch | lot/src/macos/seatbelt.rs | 193 | NIT | Comment says "most-specific-match-wins" but SBPL uses last-match-wins. Test comment (line 662) and DESIGN.md (line 123) are correct. Only line 193 wrong. |
| 31 | Doc-Mismatch | lot/src/command.rs | 23 | NIT | Field comment says "Platform essentials are always included." On Unix only `PATH` is injected if missing. On Windows nothing is injected; empty env -> null pointer -> child inherits parent's full environment. Comment is misleading. |

---

## Group 11 — Documentation and design doc mismatches

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 32 | Doc-Mismatch | docs/DESIGN.md | 250-262 | NIT | Graceful Degradation table missing `Unsupported` error variant (returned when platform lacks a required mechanism). `Timeout` and `Io` are runtime/generic errors, not degradation scenarios — correctly excluded. |
| 33 | Doc-Mismatch | lot/src/policy_builder.rs | 13-19, 83-84 | NIT | `read_path()` doc says "same-or-lower privilege sets" (plural), but read is the lowest — only read set affected. Vacuously true. |

---

## Group 12 — Separation of concerns

Large monolithic functions and mixed responsibilities.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 34 | Separation | lot/src/linux/namespace.rs | 1-983 | NIT | 983-line file handles 4 concerns but only mount namespace setup (~200 lines) is large; capability probing, user NS mapping (~12 lines), and pivot_root (~10 lines) are trivial. |
| 35 | Separation | lot/src/unix.rs | 259-485 | NIT | `read_two_fds` conflates poll event loop with data accumulation. `check_child_error_pipe` merges pipe reading, protocol decoding, and child reap/cleanup. |
| 36 | Separation | lot/src/linux/mod.rs | 581-608 | NIT | `test_helpers` module has generic fd utilities that aren't Linux-specific. |
| 37 | Separation | lot/src/linux/namespace.rs | 91-174 | NIT | `mount_system_paths` mixes path classification, mount execution, symlink creation, and network-policy-aware `/etc` mounting. |
| 38 | Separation | lot/src/macos/mod.rs | 46-215 | NIT | `spawn` is 170-line monolith. |

---

## Group 13 — Broad architectural simplification

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 39 | Simplification-Broad | policy_builder.rs, policy.rs, lib.rs | policy_builder.rs:278, lib.rs:205 | NIT | Double validation: `build()` calls `validate()`, then `spawn()` calls `validate()` again. Intentional — `spawn()` validates because callers may construct policies via `SandboxPolicy::new()` directly, bypassing the builder. |

---

## Group 14 — Inconsistent errno capture in child_bail! macro

Style inconsistency: some call sites pass raw errno dereference directly as a macro argument while others save to a local first.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 40 | Naming | lot/src/linux/mod.rs | 454 | NIT | `*libc::__errno_location()` passed directly to `child_bail!`. Not unsound — macro textual substitution places it inside the `unsafe` block — but inconsistent with other call sites that save errno to a local first (e.g., line 289). |
| 41 | Naming | lot/src/macos/mod.rs | 120, 161, 178 | NIT | Same inconsistency with `*libc::__error()`. Three call sites. |

---

## Group 15 — Naming: functions that don't match behavior

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 42 | Naming | lot/src/unix.rs | 252, 696-703, 621-630 | NIT | `close_pipe_fds` is generic not pipe-specific. `send_sigkill` name suggests fire-and-forget. `validate_kill_pid` returns `Option` not `Result`. |
| 43 | Naming | lot/src/linux/mod.rs | 104, 546, 581-608 | NIT | `close_inherited_fds` closes ALL fds not just inherited. `kill_and_cleanup` closes fds before killing. `write_fd` discards errors. |
| 44 | Naming | lot/src/linux/namespace.rs | 91-95, 298-299, 490-520 | NIT | `mount_system_paths` also creates symlinks. `execute_pivot_root` does pivot+chdir+umount+rmdir. `parse_submounts` includes prefix mount. |

---

## Group 16 — Simplification: duplicated platform code patterns

Repeated identical patterns across platform backends that could be extracted.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 45 | Simplification | lot/src/linux/namespace.rs | 193-216 | NIT | `mount_policy_paths` has three identical loops differing only in iterator and bind function. Could be single loop over `(iterator, mount_fn)` tuples. Three explicit loops (~7 lines each) is clear and readable. |
| 46 | Simplification | lot/src/unix.rs | 34-68 | NIT | `.map_err(...)` repeated 5 times for `CString::new` in `prepare_prefork`. A local helper would eliminate repetition. |
| 47 | Simplification | lot/src/macos/seatbelt.rs | 109-123 | NIT | Three separate loops for read/write/exec paths emitting identical `file-read-metadata` rules. |

---

## Group 17 — Simplification: policy and builder duplication

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 48 | Simplification | lot/src/policy.rs | 240-258 | NIT | `all_paths` and `grant_paths` have nearly identical bodies — only difference is whether `deny_paths` is chained. |
| 49 | Simplification | lot/src/policy.rs | 173-211 | NIT | `validate_deny_paths` takes three separate grant-path slices, immediately chains them. Could accept single pre-chained `&[PathBuf]`. |
| 50 | Simplification | lot/src/policy_builder.rs | 90-102, 115-129, 142-152 | NIT | `read_path`, `write_path`, `exec_path` implement same pattern differing only in which sets to check/prune. Could be single private method. |
| 51 | Simplification | lot/src/policy_builder.rs | 288-346 | NIT | `platform_exec_paths` and `platform_lib_paths` allocate `Vec<PathBuf>` of static strings. Could return arrays or static slices. |
| 52 | Simplification | lot/src/policy_builder.rs | 177-185 | NIT | `deny_paths` is a thin loop wrapper. No batch methods for read/write/exec. Adds API surface without meaningful value. |
| 53 | Simplification | lot/src/policy.rs | 215-234 | NIT | `canonicalize_collect` and `collect_validation_error` catch-all `Err(e)` arm is dead code — only `InvalidPolicy` is ever produced. |
| 54 | Simplification | lot/src/policy.rs | 426-436 | NIT | `valid_policy` helper used only once. ~20 tests construct `SandboxPolicy` with same boilerplate fields. A builder helper would eliminate repetition. |
| 55 | Simplification | lot/src/policy.rs | 447-472, 1004-1020 | NIT | `empty_policy_rejected` and `empty_policy_error_mentions_at_least_one_path` test identical setup, just different assertions. Could be merged. |

---

## Group 18 — Simplification: minor code-level cleanup

Small redundancies and unnecessary allocations.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 56 | Simplification | lot/src/macos/seatbelt.rs | 230-261 | NIT | `collect_ancestor_dirs` builds `policy_paths` HashSet and removes from `ancestors`. Removal loop has no effect (redundant, not dead). |
| 57 | Simplification | lot/src/unix.rs | 97-106 | NIT | `CString::new("/dev/null")` can never fail (no interior NUL). Dead error path. |
| 58 | Simplification | lot/src/unix.rs | 252-257 | NIT | `close_pipe_fds` duplicates iteration pattern already in `UnixSandboxedChild::close_fds`. |
| 59 | Simplification | lot/src/unix.rs | 273-307 | NIT | `read_two_fds` rebuilds `pollfds` and `fd_buffer_id` arrays every iteration. |
| 60 | Simplification | lot/src/linux/namespace.rs | 331-354 | NIT | `mount_tmpfs_with` allocates `CString` for literal `"tmpfs"` on every call. |
| 61 | Simplification | lot/src/linux/namespace.rs | 293-300 | NIT | `pivot_root` and `mount_proc_in_new_root` are one-line wrappers. Pure indirection. |
| 62 | Simplification | lot/src/macos/mod.rs | 221-261 | NIT | `MacosSandboxedChild` single-field newtype with no macOS-specific logic beyond delegation. `kill_and_cleanup` body identical to `Drop::drop`. |
| 63 | Simplification | lot/src/env_check.rs | 23-40 | NIT | `is_dir_accessible` accepts separate `canon_grants` and `canon_implicit` slices checked with identical logic. Could accept single combined slice. |
| 64 | Simplification | lot/src/path_util.rs | 16-26 | NIT | `is_descendant_or_equal` uses two-phase canonicalize-then-fallback. Could call `canonicalize_existing_prefix` unconditionally. |
| 65 | Simplification | lot/src/unix.rs | 636-670 | NIT | `delegate_unix_child_methods!` macro generates 8 trivial delegation methods. A `Deref` impl or trait would be more idiomatic. |

---

## Group 19 — Simplification: test boilerplate reduction

Repeated test boilerplate across test modules.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 66 | Simplification | lot/src/linux/mod.rs | 751-898 | NIT | Four `close_inherited_fds_*` tests share identical boilerplate. A shared helper would eliminate ~120 lines. |
| 67 | Simplification | lot/src/linux/seccomp.rs | 459-690 | NIT | 8 test child bodies share identical boilerplate. |
| 68 | Simplification | lot/src/error.rs | 41-113 | NIT | Six separate single-assertion tests verify `thiserror`'s `#[error("...")]` expansion. Could be parameterized or removed. |
| 69 | Simplification | lot/src/path_util.rs | 192-394 | NIT | `normalize_lexical` and `strict_parent_*` tests repeat `#[cfg]` gating for input/output. Helper or macro would eliminate ~60 lines. |
| 70 | Simplification | lot/src/env_check.rs | 445-474 | NIT | Tests use `std::slice::from_ref(&grant)` instead of simpler `&[grant]`. |

---

## Group 20 — Remaining NIT-level test coverage gaps

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 71 | Testing | lot/src/lib.rs | 235-244 | NIT | `cleanup_stale` on non-Windows is a no-op. No test verifies this path. |
| 72 | Testing | lot/src/lib.rs | 569-592 | NIT | `kill_by_pid` tests only verify absence of panics. |
| 73 | Testing | lot/src/policy.rs | 109-145 | NIT | `check_cross_overlap` with `AllowChildUnderParent` tested only indirectly. |
| 74 | Testing | lot/src/policy.rs | 148-169 | NIT | No test for intra-overlap within `read_paths` or `write_paths`. |
| 75 | Testing | lot/src/policy_builder.rs | 257-260 | NIT | `sentinel_dir()` has no test coverage. |
| 76 | Testing | lot/src/env_check.rs | 53, 77 | NIT | `validate_env_accessibility` has hidden dependency on host environment. |
| 77 | Testing | lot/src/env_check.rs | 161-195 | NIT | No test for first-match semantics with duplicate keys. |

