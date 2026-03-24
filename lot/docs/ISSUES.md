# Issues

Generated from audit findings: 2026-03-24

100 active findings. 0 MUST FIX. Groups ordered by impact (NON-CRITICAL first, then NIT).

---

## Group 3 — Silent failures in kill/signal/cleanup paths

Process and cgroup cleanup silently discards errors. Leaked processes or cgroups go undiagnosed.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 6 | Error-Handling | lot/src/linux/cgroup.rs | 237 | NON-CRITICAL | `libc::kill()` return silently discarded in `signal_all()` fallback. `EPERM` failure undiagnosed. |
| 7 | Error-Handling | lot/src/linux/cgroup.rs | 308 | NON-CRITICAL | `fs::remove_dir` failure silently discarded via `let _ =`. Leaks cgroup directory with no diagnostic. |
| 8 | Error-Handling | lot/src/macos/mod.rs | 233-237 | NON-CRITICAL | `kill_and_cleanup` always returns `Ok(())` even if `kill_and_reap` fails internally. |
| 9 | Error-Handling | lot/src/linux/mod.rs | 569 | NIT | `kill_by_pid` ignores `libc::kill` return. Cannot distinguish success from permission denied. |
| 10 | Error-Handling | lot/src/macos/mod.rs | 244-254 | NIT | `kill_by_pid` silently discards `libc::kill` return. Permission errors invisible. |

---

## Group 4 — Missing test coverage: security-critical paths

Sandbox enforcement code with zero test coverage. Regressions in these paths could silently weaken or break the sandbox.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 11 | Testing | lot/src/linux/namespace.rs | 83-289 | NON-CRITICAL | No test for `setup_user_namespace`, `mount_system_paths`, `mount_policy_paths`, `mount_deny_paths`, `setup_mount_namespace`. Security-sensitive orchestration ordering untested. |
| 12 | Testing | lot/src/linux/seccomp.rs | 229-242 | NON-CRITICAL | No test for disallowed ioctl request number. Bug allowing all ioctls would go undetected. |
| 13 | Testing | lot/src/linux/seccomp.rs | 321-344 | NON-CRITICAL | Only `socket` tested for network deny. `connect`, `bind`, `sendto` omission not caught. |
| 14 | Testing | lot/src/linux/namespace.rs | 37-40 | NON-CRITICAL | No test for `is_apparmor_restricted`. Regression silently disables sandboxing on AppArmor systems. |
| 15 | Testing | lot/src/macos/seatbelt.rs | 335-362 | NON-CRITICAL | `apply_profile` has zero test coverage. Contains FFI call, error branching, CString conversion. |
| 16 | Testing | lot/src/linux/mod.rs | 280-282 | NON-CRITICAL | No test for `allow_network() == true` path. |
| 17 | Testing | lot/tests/integration.rs | entire file | NIT | No test for `allow_network(true)`. |

---

## Group 5 — TOCTOU in namespace mount point setup

Race condition in mount namespace setup path.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 18 | Correctness | lot/src/linux/namespace.rs | 247-263 | NON-CRITICAL | TOCTOU window: `/tmp/lot-newroot-{pid}` is in host `/tmp`. Between `rmdir` and `mkdir_p`+`mount_tmpfs`, another process could plant a symlink or directory. Impact mitigated: code runs after `unshare(CLONE_NEWNS)` so mount operations are namespace-private, `mount_tmpfs` hides planted content, `mkdir` does not follow symlinks. |

---

## Group 6 — Path canonicalization silent failures

Canonicalization errors silently swallowed. Could cause incorrect path comparisons affecting sandbox boundary decisions.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 19 | Error-Handling | lot/src/path_util.rs | 33-34 | NON-CRITICAL | `is_strict_parent_of` silently falls back to uncanonicalized path on `canonicalize_existing_prefix` failure. Caller cannot distinguish "canonicalization succeeded, paths unrelated" from "canonicalization failed, compared raw inputs." |
| 20 | Error-Handling | lot/src/path_util.rs | 52 | NON-CRITICAL | `canonicalize_existing_prefix` silently discards `std::fs::canonicalize` errors per iteration. Permission-denied on existing path is swallowed. |
| 21 | Separation-Broad | policy_builder.rs, policy.rs, path_util.rs | multiple | NON-CRITICAL | Four different canonicalization functions across three files with different error handling. |

---

## Group 7 — Missing test coverage: lifecycle and resource management

Lifecycle operations (stdio setup, resource limits, timeout cleanup) lack tests.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 22 | Testing | lot/src/unix.rs | 512-566 | NON-CRITICAL | `setup_stdio_fds` has no test coverage. Contains non-trivial fd-aliasing logic. The aliasing case (same fd for stdout and stderr) has zero coverage. |
| 23 | Testing | lot/src/unix.rs | 572-604 | NON-CRITICAL | `apply_resource_limits` has no test. Only `set_rlimit` has a single no-op macOS test. |
| 24 | Testing | lot/src/unix.rs | 623-630 | NON-CRITICAL | `validate_kill_pid` tests gated on `#[cfg(feature = "tokio")]`. If CI doesn't run with tokio feature, tests silently skipped. |
| 25 | Testing | lot/src/lib.rs | 440-480 | NON-CRITICAL | `wait_with_output_timeout` (tokio feature) has no test. Contains nontrivial logic. |
| 26 | Testing | lot/src/linux/cgroup.rs | 200-240 | NON-CRITICAL | `signal_all()` fallback path (per-PID SIGKILL) has no test. |
| 27 | Testing | lot/tests/integration.rs | 1503-1654 | NON-CRITICAL | Tokio timeout tests don't verify child process cleanup after timeout. |
| 28 | Testing | lot/tests/integration.rs | 440-505 | NON-CRITICAL | Drop cleanup tests: Windows only checks `cleanup_stale().is_ok()`. Linux checks `/proc/{pid}` for echo that likely already exited. |

---

## Group 8 — Weak/incomplete test assertions

Tests that pass trivially or don't assert the right things.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 29 | Testing | lot/src/unix.rs | 1009-1017 | NON-CRITICAL | `close_if_not_std_skips_standard_fds` has no assertion that fds 0/1/2 remain open. Test proves nothing. |
| 30 | Testing | lot/src/lib.rs | 503-526 | NON-CRITICAL | Platform probe tests only assert expected-true fields. None verify other-platform fields are false. |
| 31 | Testing | lot/src/lib.rs | 202-222 | NON-CRITICAL | No test covers spawn() policy validation path. |
| 32 | Testing | lot/src/policy_builder.rs | 908-933 | NON-CRITICAL | `include_platform_exec_paths_succeeds` and `include_platform_lib_paths_succeeds` do not assert the convenience methods actually added paths. |
| 33 | Testing | lot/src/linux/mod.rs | 701-722 | NON-CRITICAL | `spawn_network_isolated` asserts absence of `eth0`/`wlan0` but modern distros use predictable naming. Test passes trivially. |
| 34 | Testing | lot/src/linux/cgroup.rs | 317-319 | NON-CRITICAL | `require_cgroups()` causes tests to pass on non-cgroup environments rather than skip. |
| 35 | Testing | lot/tests/integration.rs | 1281-1334 | NON-CRITICAL | macOS memory limit test silently returns early on `setrlimit` failure. Reports as passed, not skipped. |
| 36 | Testing | lot/tests/integration.rs | 793-863 | NON-CRITICAL | Unix `test_deny_path_blocks_execution` doesn't assert `!status.success()`. Only checks stdout content. |
| 37 | Testing | lot/src/path_util.rs | 14-36 | NON-CRITICAL | `is_descendant_or_equal` is `#[cfg(test)]` only. Production uses `is_strict_parent_of` which has only 4 tests with non-existent paths. |
| 38 | Testing | lot/src/path_util.rs | 48-68 | NON-CRITICAL | `canonicalize_existing_prefix` has no test for symlinks in the existing prefix — the function's stated purpose. |
| 39 | Testing | lot/src/env_check.rs | 89 | NON-CRITICAL | Only `TEMP` exercised in tests. No test sets `TMP` or `TMPDIR` independently. |

---

## Group 9 — Error handling in seccomp and fork paths

Errors from waitpid and seccomp enforcement silently dropped.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 40 | Error-Handling | lot/src/linux/seccomp.rs | 447 | NON-CRITICAL | `fork_with_seccomp` never checks `waitpid` return value or child exit status. SIGSYS from seccomp goes unnoticed. |
| 41 | Error-Handling | lot/src/unix.rs | 377 | NIT | `child_bail` discards `libc::write` return. If broken pipe, parent sees EOF and concludes success. Defensible since `_exit(1)` follows. |

---

## Group 10 — Simplification: duplicated platform code patterns

Repeated identical patterns across platform backends that could be extracted.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 42 | Simplification | lot/src/linux/namespace.rs | 193-216 | NON-CRITICAL | `mount_policy_paths` has three identical loops differing only in iterator and bind function. Could be single loop over `(iterator, mount_fn)` tuples. |
| 43 | Simplification | lot/src/unix.rs | 34-68 | NON-CRITICAL | `.map_err(...)` repeated 5 times for `CString::new` in `prepare_prefork`. A local helper would eliminate repetition. |
| 44 | Simplification | lot/src/linux/cgroup.rs | 34-65 | NON-CRITICAL | `has_writable_delegation` duplicates subtree_control check logic for parent and current cgroup. Could extract helper. |
| 45 | Simplification | lot/src/macos/seatbelt.rs | 109-123 | NIT | Three separate loops for read/write/exec paths emitting identical `file-read-metadata` rules. |

---

## Group 11 — Placement: macOS code in shared unix.rs

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 46 | Placement | lot/src/unix.rs | 568-603 | NON-CRITICAL | macOS-specific `set_rlimit`/`apply_resource_limits` in shared unix.rs. Only used by macOS backend. |

---

## Group 12 — Documentation and design doc mismatches

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 47 | Doc-Mismatch | docs/DESIGN.md | 250-262 | NON-CRITICAL | Graceful Degradation table missing `Unsupported`, `Timeout`, and `Io` error variants. |
| 48 | Doc-Mismatch | lot/src/policy_builder.rs | 13-19, 83-84 | NIT | `read_path()` doc says "same-or-lower privilege sets" (plural), but read is the lowest — only read set affected. Vacuously true. |
| 49 | Doc-Mismatch | docs/DESIGN.md | 13 | NIT | Describes `policy_builder.rs` as "auto-canonicalization, platform defaults" but omits overlap deduction. |

---

## Group 14 — Separation of concerns

Large monolithic functions and mixed responsibilities.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 58 | Separation | lot/src/linux/namespace.rs | 1-983 | NON-CRITICAL | 983-line file handles 4 distinct concerns: capability probing, user namespace mapping, mount namespace construction (~500 lines), pivot_root. |
| 59 | Separation | lot/src/unix.rs | 259-485 | NIT | `read_two_fds` conflates poll event loop with data accumulation. `check_child_error_pipe` merges pipe reading, protocol decoding, and child reap/cleanup. |
| 60 | Separation | lot/src/linux/mod.rs | 165-487 | NIT | `spawn` is ~320-line monolith. |
| 61 | Separation | lot/src/linux/mod.rs | 581-608 | NIT | `test_helpers` module has generic fd utilities that aren't Linux-specific. |
| 62 | Separation | lot/src/linux/namespace.rs | 91-174 | NIT | `mount_system_paths` mixes path classification, mount execution, symlink creation, and network-policy-aware `/etc` mounting. |
| 63 | Separation | lot/src/macos/mod.rs | 46-215 | NIT | `spawn` is 170-line monolith. |
| 64 | Separation-Broad | windows/acl_helpers.rs, sddl.rs, traverse_acl.rs | multiple | NIT | Three files with overlapping DACL manipulation responsibility. |

---

## Group 15 — Error handling in test helpers

Test helpers discard errors, producing confusing failures or false passes.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 65 | Error-Handling | lot/src/unix.rs | 1579 | NON-CRITICAL | `set_rlimit_nofile_succeeds` doesn't check `getrlimit` return value. If it fails, test sets RLIMIT_NOFILE to 0 — tests wrong scenario. |
| 66 | Error-Handling | lot/src/unix.rs | 1148-1156 | NIT | Test helper `fork_pipe_writer` discards write return value. Failed write produces empty pipe indistinguishable from success. |
| 67 | Error-Handling | lot/src/unix.rs | 1540-1549 | NIT | Test child branch discards `libc::write` return for stdout/stderr. Failures surface as confusing assertion. |
| 68 | Error-Handling | lot/src/linux/mod.rs | 792-794 | NIT | `waitpid` return value unchecked in 4 test functions. Status remains 0 on failure. |
| 69 | Error-Handling | lot/src/linux/namespace.rs | 183-185 | NIT | `create_mount_target` silently skips parent creation when path not valid UTF-8. Falls through to confusing error. |
| 70 | Error-Handling | lot/src/linux/namespace.rs | 399 | NIT | `create_mount_point_file` does not check `libc::close(fd)` return value. |

---

## Group 1 — Inconsistent errno capture in child_bail! macro

Style inconsistency: some call sites pass raw errno dereference directly as a macro argument while others save to a local first.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 1 | Naming | lot/src/linux/mod.rs | 454 | NIT | `*libc::__errno_location()` passed directly to `child_bail!`. Not unsound — macro textual substitution places it inside the `unsafe` block — but inconsistent with other call sites that save errno to a local first (e.g., line 289). |
| 2 | Naming | lot/src/macos/mod.rs | 120, 161, 178 | NIT | Same inconsistency with `*libc::__error()`. Three call sites. |

---

## Group 2 — Incorrect comments

Wrong descriptions of sandbox rule evaluation, environment behavior, and cgroup model.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 3 | Doc-Mismatch | lot/src/macos/seatbelt.rs | 193 | NIT | Comment says "most-specific-match-wins" but SBPL uses last-match-wins. Test comment (line 662) and DESIGN.md (line 123) are correct. Only line 193 wrong. |
| 4 | Doc-Mismatch | lot/src/command.rs | 23 | NIT | Field comment says "Platform essentials are always included." On Unix only `PATH` is injected if missing. On Windows nothing is injected; empty env → null pointer → child inherits parent's full environment. Comment is misleading. |
| 5 | Doc-Mismatch | lot/src/linux/cgroup.rs | 82-84 | NIT | Struct doc says "subdirectory under current process's cgroup subtree" but implementation creates under parent (sibling model). Method doc (line 93) and DESIGN.md correct; struct comment wrong. |

---

## Group 13 — Simplification: policy and builder duplication

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 50 | Simplification | lot/src/policy.rs | 240-258 | NIT | `all_paths` and `grant_paths` have nearly identical bodies — only difference is whether `deny_paths` is chained. |
| 51 | Simplification | lot/src/policy.rs | 173-211 | NIT | `validate_deny_paths` takes three separate grant-path slices, immediately chains them. Could accept single pre-chained `&[PathBuf]`. |
| 52 | Simplification | lot/src/policy_builder.rs | 90-102, 115-129, 142-152 | NIT | `read_path`, `write_path`, `exec_path` implement same pattern differing only in which sets to check/prune. Could be single private method. |
| 53 | Simplification | lot/src/policy_builder.rs | 288-346 | NIT | `platform_exec_paths` and `platform_lib_paths` allocate `Vec<PathBuf>` of static strings. Could return arrays or static slices. |
| 54 | Simplification | lot/src/policy_builder.rs | 177-185 | NIT | `deny_paths` is a thin loop wrapper. No batch methods for read/write/exec. Adds API surface without meaningful value. |
| 55 | Simplification | lot/src/policy.rs | 215-234 | NIT | `canonicalize_collect` and `collect_validation_error` catch-all `Err(e)` arm is dead code — only `InvalidPolicy` is ever produced. |
| 56 | Simplification | lot/src/policy.rs | 426-436 | NIT | `valid_policy` helper used only once. ~20 tests construct `SandboxPolicy` with same 7 boilerplate fields. A builder helper would eliminate repetition. |
| 57 | Simplification | lot/src/policy.rs | 447-472, 1004-1020 | NIT | `empty_policy_rejected` and `empty_policy_error_mentions_at_least_one_path` test identical setup, just different assertions. Could be merged. |

---

## Group 16 — Simplification: test boilerplate reduction

Repeated test boilerplate across test modules.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 71 | Simplification | lot/src/linux/mod.rs | 751-898 | NIT | Four `close_inherited_fds_*` tests share identical boilerplate. A shared helper would eliminate ~120 lines. |
| 72 | Simplification | lot/src/linux/seccomp.rs | 459-690 | NIT | 8 test child bodies share identical boilerplate. |
| 73 | Simplification | lot/src/error.rs | 41-113 | NIT | Six separate single-assertion tests verify `thiserror`'s `#[error("...")]` expansion. Could be parameterized or removed. |
| 74 | Simplification | lot/src/path_util.rs | 192-394 | NIT | `normalize_lexical` and `strict_parent_*` tests repeat `#[cfg]` gating for input/output. Helper or macro would eliminate ~60 lines. |
| 75 | Simplification | lot/src/env_check.rs | 445-474 | NIT | Tests use `std::slice::from_ref(&grant)` instead of simpler `&[grant]`. |

---

## Group 17 — Naming: functions that don't match behavior

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 76 | Naming | lot/src/unix.rs | 252, 696-703, 621-630 | NIT | `close_pipe_fds` is generic not pipe-specific. `send_sigkill` name suggests fire-and-forget. `validate_kill_pid` returns `Option` not `Result`. |
| 77 | Naming | lot/src/linux/mod.rs | 104, 546, 581-608 | NIT | `close_inherited_fds` closes ALL fds not just inherited. `kill_and_cleanup` closes fds before killing. `write_fd` discards errors. |
| 78 | Naming | lot/src/linux/namespace.rs | 91-95, 298-299, 490-520 | NIT | `mount_system_paths` also creates symlinks. `execute_pivot_root` does pivot+chdir+umount+rmdir. `parse_submounts` includes prefix mount. |
| 79 | Naming | lot/src/linux/cgroup.rs | 200 | NIT | `signal_all` always sends SIGKILL but name implies generic signal. |

---

## Group 18 — Simplification: minor code-level cleanup

Small redundancies and unnecessary allocations.

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 80 | Simplification | lot/src/macos/seatbelt.rs | 230-261 | NIT | `collect_ancestor_dirs` builds `policy_paths` HashSet and removes from `ancestors`. Removal loop has no effect (redundant, not dead). |
| 81 | Simplification | lot/src/unix.rs | 97-106 | NIT | `CString::new("/dev/null")` can never fail (no interior NUL). Dead error path. |
| 82 | Simplification | lot/src/unix.rs | 252-257 | NIT | `close_pipe_fds` duplicates iteration pattern already in `UnixSandboxedChild::close_fds`. |
| 83 | Simplification | lot/src/unix.rs | 273-307 | NIT | `read_two_fds` rebuilds `pollfds` and `fd_buffer_id` arrays every iteration. |
| 84 | Simplification | lot/src/linux/cgroup.rs | 266-298 | NIT | `procs_path` recomputed on every drain loop iteration. |
| 85 | Simplification | lot/src/linux/namespace.rs | 331-354 | NIT | `mount_tmpfs_with` allocates `CString` for literal `"tmpfs"` on every call. |
| 86 | Simplification | lot/src/linux/namespace.rs | 293-300 | NIT | `pivot_root` and `mount_proc_in_new_root` are one-line wrappers. Pure indirection. |
| 87 | Simplification | lot/src/macos/mod.rs | 221-261 | NIT | `MacosSandboxedChild` single-field newtype with no macOS-specific logic beyond delegation. `kill_and_cleanup` body identical to `Drop::drop`. |
| 88 | Simplification | lot/src/env_check.rs | 23-40 | NIT | `is_dir_accessible` accepts separate `canon_grants` and `canon_implicit` slices checked with identical logic. Could accept single combined slice. |
| 89 | Simplification | lot/src/path_util.rs | 16-26 | NIT | `is_descendant_or_equal` uses two-phase canonicalize-then-fallback. Could call `canonicalize_existing_prefix` unconditionally. |
| 90 | Simplification | lot/src/unix.rs | 636-670 | NIT | `delegate_unix_child_methods!` macro generates 8 trivial delegation methods. A `Deref` impl or trait would be more idiomatic. |

---

## Group 19 — Remaining NIT-level test coverage gaps

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 91 | Testing | lot/src/lib.rs | 235-244 | NIT | `cleanup_stale` on non-Windows is a no-op. No test verifies this path. |
| 92 | Testing | lot/src/lib.rs | 569-592 | NIT | `kill_by_pid` tests only verify absence of panics. |
| 93 | Testing | lot/src/policy.rs | 109-145 | NIT | `check_cross_overlap` with `AllowChildUnderParent` tested only indirectly. |
| 94 | Testing | lot/src/policy.rs | 148-169 | NIT | No test for intra-overlap within `read_paths` or `write_paths`. |
| 95 | Testing | lot/src/policy_builder.rs | 257-260 | NIT | `sentinel_dir()` has no test coverage. |
| 96 | Testing | lot/src/env_check.rs | 53, 77 | NIT | `validate_env_accessibility` has hidden dependency on host environment. |
| 97 | Testing | lot/src/env_check.rs | 161-195 | NIT | No test for first-match semantics with duplicate keys. |
| 98 | Testing | lot/src/linux/mod.rs | 546-555 | NIT | `kill_and_cleanup` has no test. |
| 99 | Testing | lot/src/linux/mod.rs | 185-197 | NIT | No test covers cgroup integration path in `spawn`. |
| 100 | Testing | lot/src/linux/mod.rs | 948-967 | NIT | `drop_kills_long_running_child` uses 100ms sleep — race condition on loaded systems. |
| 101 | Testing | lot/src/linux/namespace.rs | 410-464 | NIT | No test for `bind_mount` EINVAL skip logic. |
| 102 | Testing | lot/src/linux/cgroup.rs | 69-78, 244-256 | NIT | `current_cgroup_path()` and `pid_in_cgroup()` parsing has no unit test with controlled input. |
| 103 | Testing | lot/src/macos/mod.rs | 395-429 | NIT | `spawn_invalid_cwd` asserts `is_err()` without verifying error variant. `spawn_read_outside_sandbox_blocked` doesn't assert exit status. |
| 104 | Testing | lot/src/macos/mod.rs | 233-254 | NIT | `kill_and_cleanup` and `kill_by_pid` both untested. |
| 105 | Testing | lot/src/macos/seatbelt.rs | 288 | NIT | No test for empty string path in `escape_sbpl_path`. |
| 106 | Testing | lot/src/macos/seatbelt.rs | 403-457 | NIT | Profile tests silently depend on non-existence of paths like `/tmp/test_read`. |

---

## Group 20 — Broad architectural simplification

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 107 | Simplification-Broad | policy_builder.rs, policy.rs, lib.rs | policy_builder.rs:278, lib.rs:205 | NIT | Double validation: `build()` calls `validate()`, then `spawn()` calls `validate()` again. Intentional — `spawn()` validates because callers may construct policies via `SandboxPolicy::new()` directly, bypassing the builder. |

---

## Group 21 — Remaining NIT items

| # | Category | File | Line(s) | Severity | Description |
|---|----------|------|---------|----------|-------------|
| 108 | Correctness | lot/src/unix.rs | 519-526 | NIT | `effective_fd` returns first match in redirected array. Fragile if calling pattern changes, though safe with current 3-step logic. |
