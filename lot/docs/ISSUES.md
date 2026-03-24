# Issues

Generated from audit findings: 2026-03-24

88 active findings. 0 MUST FIX, 10 NON-CRITICAL, 78 NIT. Groups ordered by impact (NON-CRITICAL first, then NIT).

Review notes appended per group in STATUS.md.

---

## Group 2 — Weak/incomplete test assertions

Tests that pass trivially or don't assert the right things.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 8 | Testing | lot/src/unix.rs | 1009-1017 | NIT | `close_if_not_std_skips_standard_fds` has no explicit assertion that fds 0/1/2 remain open. Guard is 3 lines (`if fd > 2`); companion test `close_if_not_std_closes_non_standard_fd` validates fd closure robustly via `fcntl`/EPIPE checks. |
| 9 | Testing | lot/src/lib.rs | 202-222 | NIT | No test calls `spawn()` with an invalid policy to verify error propagation. `validate()` is well-tested separately in policy.rs; risk is low. |
| 10 | Testing | lot/src/policy_builder.rs | 908-933 | NON-CRITICAL | `include_platform_exec_paths_succeeds` and `include_platform_lib_paths_succeeds` do not assert the convenience methods actually added paths. Assertions are satisfied by manually-added paths, not the methods under test. |
| 11 | Testing | lot/src/linux/mod.rs | 701-722 | NON-CRITICAL | `spawn_network_isolated` asserts absence of `eth0`/`wlan0` but modern distros use predictable naming (e.g., `enp0s3`). Test passes trivially on those systems. Should assert only `lo` is present. |
| 13 | Testing | lot/tests/integration.rs | 793-863 | NON-CRITICAL | Unix path of `test_deny_path_blocks_execution` doesn't assert `!status.success()`. Only checks stdout content. Windows path does assert exit status correctly. |
| 14 | Testing | lot/src/path_util.rs | 14-26, 28-36 | NIT | `is_descendant_or_equal` (14-26) is `#[cfg(test)]` only. Production uses `is_strict_parent_of` (28-36) which has 4 tests using non-existent paths (exercising lexical fallback). Core logic (`starts_with` + inequality) is well-covered; canonicalization tested separately. |
| 15 | Testing | lot/src/path_util.rs | 48-68 | NIT | `canonicalize_existing_prefix` has no direct test for symlinks in the existing prefix. Indirect symlink coverage exists via `descendant_or_equal_through_symlink` test (line 226), which calls `canonicalize_existing_prefix` internally. |
| 16 | Testing | lot/src/env_check.rs | 89 | NIT | Only `TEMP` exercised in tests. No test sets `TMP` or `TMPDIR` independently. Trivial loop — all three keys share identical handling. |

---

## Group 3 — Missing test coverage: lifecycle and resource management

Lifecycle operations (stdio setup, resource limits, timeout cleanup) lack tests.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 17 | Testing | lot/src/unix.rs | 512-566 | NON-CRITICAL | `setup_stdio_fds` has no test coverage. Contains non-trivial fd-aliasing logic. The aliasing case (same fd for stdout and stderr) has zero coverage. |
| 18 | Testing | lot/src/unix.rs | 589-604 | NON-CRITICAL | `apply_resource_limits` has no test. Only `set_rlimit` has a single macOS test (`set_rlimit_nofile_succeeds`) that tests a different resource (NOFILE). |
| 19 | Testing | lot/src/linux/cgroup.rs | 200-240 | NON-CRITICAL | `signal_all()` fallback path (per-PID SIGKILL) has no test. On kernels with `cgroup.kill` (5.14+), only the atomic path runs; fallback is untested. |
| 20 | Testing | lot/tests/integration.rs | 1503-1654 | NON-CRITICAL | Tokio timeout tests don't verify child process cleanup after timeout. |
| 21 | Testing | lot/tests/integration.rs | 440-505 | NON-CRITICAL | Drop cleanup tests: Windows only checks `cleanup_stale().is_ok()`. Linux/macOS check process termination for an `echo` child that likely already exited on its own. |

---

## Group 4 — Silent failures in kill/signal/cleanup paths

Process and cgroup cleanup silently discards errors. Leaked processes or cgroups go undiagnosed.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 22 | Error-Handling | lot/src/linux/cgroup.rs | 237 | NON-CRITICAL | `libc::kill()` return silently discarded in `signal_all()` fallback. `EPERM` failure undiagnosed. |
| 23 | Error-Handling | lot/src/linux/cgroup.rs | 308 | NIT | `fs::remove_dir` failure silently discarded via `let _ =` in `Drop` impl. Cannot propagate errors from Drop; diagnostic logging is the only option. |
| 24 | Error-Handling | lot/src/linux/mod.rs | 569 | NIT | `kill_by_pid` ignores `libc::kill` return. Cannot distinguish success from permission denied. |
| 25 | Error-Handling | lot/src/macos/mod.rs | 244-254 | NIT | `kill_by_pid` silently discards `libc::kill` return. Permission errors invisible. |

---

## Group 5 — Placement: macOS code in shared unix.rs

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 26 | Placement | lot/src/unix.rs | 568-603 | NON-CRITICAL | macOS-specific `set_rlimit`/`apply_resource_limits` in shared unix.rs. Only used by macOS backend. |

---

## Group 6 — TOCTOU in namespace mount point setup

Race condition in mount namespace setup path.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 27 | Correctness | lot/src/linux/namespace.rs | 247-263 | NIT | TOCTOU window: `/tmp/lot-newroot-{pid}` is in host `/tmp`. Between `rmdir` and `mkdir_p`+`mount_tmpfs`, another process could plant a symlink or directory. Operationally harmless: `setup_mount_namespace` runs after `unshare(CLONE_NEWNS)` (mod.rs:306, after unshare at :285), so mount operations are namespace-private. `mount_tmpfs` hides any planted content, `mkdir` does not follow symlinks. |

---

## Group 7 — Path canonicalization fallback in `is_strict_parent_of`

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 28 | Error-Handling | lot/src/path_util.rs | 33-34 | NIT | `is_strict_parent_of` falls back to uncanonicalized path on `canonicalize_existing_prefix` failure. Harmless in practice: all callers are in `policy.rs` validation, where paths have already been canonicalized by `canonicalize_collect`. The fallback handles only edge cases already caught upstream. |

---

## Group 8 — Remaining correctness NIT

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 29 | Correctness | lot/src/unix.rs | 519-526 | NIT | `effective_fd` returns first match in redirected array. Fragile if calling pattern changes, though safe with current 3-step logic. |

---

## Group 9 — Error handling in fork/child paths

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 30 | Error-Handling | lot/src/linux/seccomp.rs | 447 | NIT | Test helper `fork_with_seccomp` doesn't check `waitpid` return value or child exit status. SIGSYS would not go unnoticed — child can't write "OK" to pipe, so the test assertion fails. Real issue is unchecked `waitpid` return in test helper. |
| 31 | Error-Handling | lot/src/unix.rs | 377 | NIT | `child_bail` discards `libc::write` return. If broken pipe, parent sees EOF and concludes success. Defensible since `_exit(1)` follows. |

---

## Group 10 — Error handling in test helpers

Test helpers discard errors, producing confusing failures or false passes.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 32 | Error-Handling | lot/src/unix.rs | 1148-1156 | NIT | Test helper `fork_pipe_writer` discards write return value. Failed write produces empty pipe indistinguishable from success. |
| 33 | Error-Handling | lot/src/unix.rs | 1540-1549 | NIT | Test child branch discards `libc::write` return for stdout/stderr. Failures surface as confusing assertion. |
| 34 | Error-Handling | lot/src/linux/mod.rs | 792-794 | NIT | `waitpid` return value unchecked in 4 test functions. Status remains 0 on failure. |
| 35 | Error-Handling | lot/src/linux/namespace.rs | 399 | NIT | `create_mount_point_file` does not check `libc::close(fd)` return value. Production code, not a test helper. |

---

## Group 11 — Incorrect comments

Wrong descriptions of sandbox rule evaluation, environment behavior, and cgroup model.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 36 | Doc-Mismatch | lot/src/macos/seatbelt.rs | 193 | NIT | Comment says "most-specific-match-wins" but SBPL uses last-match-wins. Test comment (line 662) and DESIGN.md (line 123) are correct. Only line 193 wrong. |
| 37 | Doc-Mismatch | lot/src/command.rs | 23 | NIT | Field comment says "Platform essentials are always included." On Unix only `PATH` is injected if missing. On Windows nothing is injected; empty env → null pointer → child inherits parent's full environment. Comment is misleading. |
| 38 | Doc-Mismatch | lot/src/linux/cgroup.rs | 82-84 | NIT | Struct doc says "subdirectory under current process's cgroup subtree" but implementation creates under parent (sibling model). Method doc (line 93) and DESIGN.md correct; struct comment wrong. |

---

## Group 12 — Documentation and design doc mismatches

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 39 | Doc-Mismatch | docs/DESIGN.md | 250-262 | NIT | Graceful Degradation table missing `Unsupported` error variant (returned when platform lacks a required mechanism). `Timeout` and `Io` are runtime/generic errors, not degradation scenarios — correctly excluded. |
| 40 | Doc-Mismatch | lot/src/policy_builder.rs | 13-19, 83-84 | NIT | `read_path()` doc says "same-or-lower privilege sets" (plural), but read is the lowest — only read set affected. Vacuously true. |

---

## Group 13 — Separation of concerns

Large monolithic functions and mixed responsibilities.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 41 | Separation | lot/src/linux/namespace.rs | 1-983 | NIT | 983-line file handles 4 concerns but only mount namespace setup (~200 lines) is large; capability probing, user NS mapping (~12 lines), and pivot_root (~10 lines) are trivial. |
| 42 | Separation | lot/src/unix.rs | 259-485 | NIT | `read_two_fds` conflates poll event loop with data accumulation. `check_child_error_pipe` merges pipe reading, protocol decoding, and child reap/cleanup. |
| 43 | Separation | lot/src/linux/mod.rs | 581-608 | NIT | `test_helpers` module has generic fd utilities that aren't Linux-specific. |
| 44 | Separation | lot/src/linux/namespace.rs | 91-174 | NIT | `mount_system_paths` mixes path classification, mount execution, symlink creation, and network-policy-aware `/etc` mounting. |
| 45 | Separation | lot/src/macos/mod.rs | 46-215 | NIT | `spawn` is 170-line monolith. |

---

## Group 14 — Broad architectural simplification

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 46 | Simplification-Broad | policy_builder.rs, policy.rs, lib.rs | policy_builder.rs:278, lib.rs:205 | NIT | Double validation: `build()` calls `validate()`, then `spawn()` calls `validate()` again. Intentional — `spawn()` validates because callers may construct policies via `SandboxPolicy::new()` directly, bypassing the builder. |

---

## Group 15 — Inconsistent errno capture in child_bail! macro

Style inconsistency: some call sites pass raw errno dereference directly as a macro argument while others save to a local first.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 47 | Naming | lot/src/linux/mod.rs | 454 | NIT | `*libc::__errno_location()` passed directly to `child_bail!`. Not unsound — macro textual substitution places it inside the `unsafe` block — but inconsistent with other call sites that save errno to a local first (e.g., line 289). |
| 48 | Naming | lot/src/macos/mod.rs | 120, 161, 178 | NIT | Same inconsistency with `*libc::__error()`. Three call sites. |

---

## Group 16 — Naming: functions that don't match behavior

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 49 | Naming | lot/src/unix.rs | 252, 696-703, 621-630 | NIT | `close_pipe_fds` is generic not pipe-specific. `send_sigkill` name suggests fire-and-forget. `validate_kill_pid` returns `Option` not `Result`. |
| 50 | Naming | lot/src/linux/mod.rs | 104, 546, 581-608 | NIT | `close_inherited_fds` closes ALL fds not just inherited. `kill_and_cleanup` closes fds before killing. `write_fd` discards errors. |
| 51 | Naming | lot/src/linux/namespace.rs | 91-95, 298-299, 490-520 | NIT | `mount_system_paths` also creates symlinks. `execute_pivot_root` does pivot+chdir+umount+rmdir. `parse_submounts` includes prefix mount. |
| 52 | Naming | lot/src/linux/cgroup.rs | 200 | NIT | `signal_all` always sends SIGKILL but name implies generic signal. |

---

## Group 17 — Simplification: duplicated platform code patterns

Repeated identical patterns across platform backends that could be extracted.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 53 | Simplification | lot/src/linux/namespace.rs | 193-216 | NIT | `mount_policy_paths` has three identical loops differing only in iterator and bind function. Could be single loop over `(iterator, mount_fn)` tuples. Three explicit loops (~7 lines each) is clear and readable. |
| 54 | Simplification | lot/src/unix.rs | 34-68 | NIT | `.map_err(...)` repeated 5 times for `CString::new` in `prepare_prefork`. A local helper would eliminate repetition. |
| 55 | Simplification | lot/src/linux/cgroup.rs | 34-65 | NIT | `has_writable_delegation` duplicates subtree_control check logic for parent and current cgroup. Could extract helper. ~30-line function; duplication is minor. |
| 56 | Simplification | lot/src/macos/seatbelt.rs | 109-123 | NIT | Three separate loops for read/write/exec paths emitting identical `file-read-metadata` rules. |

---

## Group 18 — Simplification: policy and builder duplication

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 57 | Simplification | lot/src/policy.rs | 240-258 | NIT | `all_paths` and `grant_paths` have nearly identical bodies — only difference is whether `deny_paths` is chained. |
| 58 | Simplification | lot/src/policy.rs | 173-211 | NIT | `validate_deny_paths` takes three separate grant-path slices, immediately chains them. Could accept single pre-chained `&[PathBuf]`. |
| 59 | Simplification | lot/src/policy_builder.rs | 90-102, 115-129, 142-152 | NIT | `read_path`, `write_path`, `exec_path` implement same pattern differing only in which sets to check/prune. Could be single private method. |
| 60 | Simplification | lot/src/policy_builder.rs | 288-346 | NIT | `platform_exec_paths` and `platform_lib_paths` allocate `Vec<PathBuf>` of static strings. Could return arrays or static slices. |
| 61 | Simplification | lot/src/policy_builder.rs | 177-185 | NIT | `deny_paths` is a thin loop wrapper. No batch methods for read/write/exec. Adds API surface without meaningful value. |
| 62 | Simplification | lot/src/policy.rs | 215-234 | NIT | `canonicalize_collect` and `collect_validation_error` catch-all `Err(e)` arm is dead code — only `InvalidPolicy` is ever produced. |
| 63 | Simplification | lot/src/policy.rs | 426-436 | NIT | `valid_policy` helper used only once. ~20 tests construct `SandboxPolicy` with same 7 boilerplate fields. A builder helper would eliminate repetition. |
| 64 | Simplification | lot/src/policy.rs | 447-472, 1004-1020 | NIT | `empty_policy_rejected` and `empty_policy_error_mentions_at_least_one_path` test identical setup, just different assertions. Could be merged. |

---

## Group 19 — Simplification: minor code-level cleanup

Small redundancies and unnecessary allocations.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 65 | Simplification | lot/src/macos/seatbelt.rs | 230-261 | NIT | `collect_ancestor_dirs` builds `policy_paths` HashSet and removes from `ancestors`. Removal loop has no effect (redundant, not dead). |
| 66 | Simplification | lot/src/unix.rs | 97-106 | NIT | `CString::new("/dev/null")` can never fail (no interior NUL). Dead error path. |
| 67 | Simplification | lot/src/unix.rs | 252-257 | NIT | `close_pipe_fds` duplicates iteration pattern already in `UnixSandboxedChild::close_fds`. |
| 68 | Simplification | lot/src/unix.rs | 273-307 | NIT | `read_two_fds` rebuilds `pollfds` and `fd_buffer_id` arrays every iteration. |
| 69 | Simplification | lot/src/linux/cgroup.rs | 266-298 | NIT | `procs_path` recomputed on every drain loop iteration. |
| 70 | Simplification | lot/src/linux/namespace.rs | 331-354 | NIT | `mount_tmpfs_with` allocates `CString` for literal `"tmpfs"` on every call. |
| 71 | Simplification | lot/src/linux/namespace.rs | 293-300 | NIT | `pivot_root` and `mount_proc_in_new_root` are one-line wrappers. Pure indirection. |
| 72 | Simplification | lot/src/macos/mod.rs | 221-261 | NIT | `MacosSandboxedChild` single-field newtype with no macOS-specific logic beyond delegation. `kill_and_cleanup` body identical to `Drop::drop`. |
| 73 | Simplification | lot/src/env_check.rs | 23-40 | NIT | `is_dir_accessible` accepts separate `canon_grants` and `canon_implicit` slices checked with identical logic. Could accept single combined slice. |
| 74 | Simplification | lot/src/path_util.rs | 16-26 | NIT | `is_descendant_or_equal` uses two-phase canonicalize-then-fallback. Could call `canonicalize_existing_prefix` unconditionally. |
| 75 | Simplification | lot/src/unix.rs | 636-670 | NIT | `delegate_unix_child_methods!` macro generates 8 trivial delegation methods. A `Deref` impl or trait would be more idiomatic. |

---

## Group 20 — Simplification: test boilerplate reduction

Repeated test boilerplate across test modules.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 76 | Simplification | lot/src/linux/mod.rs | 751-898 | NIT | Four `close_inherited_fds_*` tests share identical boilerplate. A shared helper would eliminate ~120 lines. |
| 77 | Simplification | lot/src/linux/seccomp.rs | 459-690 | NIT | 8 test child bodies share identical boilerplate. |
| 78 | Simplification | lot/src/error.rs | 41-113 | NIT | Six separate single-assertion tests verify `thiserror`'s `#[error("...")]` expansion. Could be parameterized or removed. |
| 79 | Simplification | lot/src/path_util.rs | 192-394 | NIT | `normalize_lexical` and `strict_parent_*` tests repeat `#[cfg]` gating for input/output. Helper or macro would eliminate ~60 lines. |
| 80 | Simplification | lot/src/env_check.rs | 445-474 | NIT | Tests use `std::slice::from_ref(&grant)` instead of simpler `&[grant]`. |

---

## Group 21 — Remaining NIT-level test coverage gaps

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 81 | Testing | lot/src/lib.rs | 235-244 | NIT | `cleanup_stale` on non-Windows is a no-op. No test verifies this path. |
| 82 | Testing | lot/src/lib.rs | 569-592 | NIT | `kill_by_pid` tests only verify absence of panics. |
| 83 | Testing | lot/src/policy.rs | 109-145 | NIT | `check_cross_overlap` with `AllowChildUnderParent` tested only indirectly. |
| 84 | Testing | lot/src/policy.rs | 148-169 | NIT | No test for intra-overlap within `read_paths` or `write_paths`. |
| 85 | Testing | lot/src/policy_builder.rs | 257-260 | NIT | `sentinel_dir()` has no test coverage. |
| 86 | Testing | lot/src/env_check.rs | 53, 77 | NIT | `validate_env_accessibility` has hidden dependency on host environment. |
| 87 | Testing | lot/src/env_check.rs | 161-195 | NIT | No test for first-match semantics with duplicate keys. |
| 88 | Testing | lot/src/linux/mod.rs | 546-555 | NIT | `kill_and_cleanup` has no test. |
| 89 | Testing | lot/src/linux/mod.rs | 185-197 | NIT | No test covers cgroup integration path in `spawn`. |
| 90 | Testing | lot/src/linux/mod.rs | 948-967 | NIT | `drop_kills_long_running_child` uses 100ms sleep — race condition on loaded systems. |
| 91 | Testing | lot/src/linux/namespace.rs | 410-464 | NIT | No test for `bind_mount` EINVAL skip logic. |
| 92 | Testing | lot/src/linux/cgroup.rs | 69-78, 244-256 | NIT | `current_cgroup_path()` and `pid_in_cgroup()` parsing has no unit test with controlled input. |
| 93 | Testing | lot/src/macos/mod.rs | 395-429 | NIT | `spawn_invalid_cwd` asserts `is_err()` without verifying error variant. `spawn_read_outside_sandbox_blocked` doesn't assert exit status. |
| 94 | Testing | lot/src/macos/mod.rs | 233-254 | NIT | `kill_and_cleanup` and `kill_by_pid` both untested. |
| 95 | Testing | lot/src/macos/seatbelt.rs | 288 | NIT | No test for empty string path in `escape_sbpl_path`. |
| 96 | Testing | lot/src/macos/seatbelt.rs | 403-457 | NIT | Profile tests silently depend on non-existence of paths like `/tmp/test_read`. |
