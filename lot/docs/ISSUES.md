# Issues

*50 findings from full project audit (2026-03-22). Grouped by co-fixability, ordered by impact.*

---

## Group 2: Windows ACL/DACL Defensiveness

### 2.1 [Medium/Correctness] `modify_dacl` missing null check — `lot/src/windows/acl_helpers.rs:170`
`modify_dacl` doesn't verify `sd` is non-null after `GetNamedSecurityInfoW`, unlike `read_dacl` which does. `ERROR_SUCCESS` guarantees valid `sd` per Win32 contract, so this is a defensive-coding inconsistency.

### 2.2 [Medium/Error-handling] `GetAce` failure silently swallowed — `lot/src/windows/acl_helpers.rs:370-372`
Returns false-negative `Ok(false)` on corrupted ACL instead of propagating an error. In practice, causes redundant (but harmless) ACE re-application.

### 2.3 [Medium/Correctness] HANDLE leak in `traverse_acl.rs` — `lot/src/windows/traverse_acl.rs:417-423`
HANDLE from `CreateFileW` not in RAII guard. Leaks on panic or early return between open and `CloseHandle`.

### 2.4 [Medium/Error-handling] NTSTATUS formatted incorrectly — `lot/src/windows/traverse_acl.rs:425-437`
Negative NTSTATUS formatted as i32 hex produces wrong output (e.g., `0xC0000022` renders as `-0x3FFFFDDE`). Should cast to u32 before formatting.

---

## Group 3: Windows AppContainer Spawn Refactoring

### 3.1 [Medium/Simplification+Separation] Manual stdio cleanup cascade — `lot/src/windows/appcontainer.rs:644-686`
Manual error-cleanup cascade for stdio handles interleaved with spawn orchestration. RAII guard would eliminate manual branches.

### 3.2 [Medium/Separation] `spawn_with_sentinel` mixes concerns — `lot/src/windows/appcontainer.rs:600-822`
Mixes attribute list lifecycle, stdio pipe resolution, and spawn orchestration in one function.

### 3.3 [Medium/Simplification] Redundant `display` clone — `lot/src/windows/appcontainer.rs:103-106`
`display` is redundant clone of `wide_name`. Same `to_wide(&name)` called twice.

### 3.4 [Medium/Simplification] Duplicated `CreateAppContainerProfile` — `lot/src/windows/appcontainer.rs:110-139`
`CreateAppContainerProfile` call duplicated verbatim in two branches.

---


## Group 5: Correctness Edge Cases

### 5.1 [Medium/Correctness] Program path not fully escaped — `lot/src/windows/cmdline.rs:38-40`
`build_command_line` quotes program path but does not escape embedded double-quotes or trailing backslashes. Practical impact limited: `"` is illegal in Windows filenames.

### 5.2 [Medium/Correctness] `cpu_secs` overflow to negative — `lot/src/windows/job.rs:87`
Values above ~922,337,203 produce negative `PerJobUserTimeLimit` after `saturating_mul(10_000_000) as i64`. Has explicit `#[allow]` and comment. No realistic input reaches the threshold.

### 5.3 [Medium/Correctness] Sentinel TOCTOU race — `lot/src/windows/sentinel.rs:278-288`
Between alive-check and restore, another process could double-restore ACLs. Impact limited: `apply_sddl` is idempotent and `delete_file` handles `NotFound`.

### 5.4 [Medium/Correctness] Malformed step wraps to `usize::MAX` — `lot/src/unix.rs:467-468`
If `step` is 0 or negative, `(step - 1) as usize` wraps. Falls through to `"unknown"` safely but loses the actual invalid step value.

### 5.5 [Medium/Correctness] `cmd_setup` uses hardcoded policy — `lot-cli/src/main.rs:189-202`
Checks prerequisites for a hardcoded minimal policy, not the user's actual policy. `lot run` validates at spawn time, so not a gap in production use.

---

## Group 6: Cross-Platform Best-Effort Kill/Cleanup Return Values

### 6.1 [Medium/Error-handling] `kill(SIGKILL)` discarded in cgroup cleanup — `lot/src/linux/cgroup.rs:226`
Best-effort fallback path; kernel kills remaining cgroup members on removal.

### 6.2 [Medium/Error-handling] `kill_by_pid` discards `kill()` — `lot/src/linux/mod.rs:569`
Documented as best-effort, used in async cancellation paths.

### 6.3 [Medium/Error-handling] `TerminateProcess` discarded — `lot/src/windows/mod.rs:105`
Consistent with cross-platform best-effort `kill_by_pid` design.

### 6.4 [Medium/Error-handling] `kill(-pid, SIGKILL)` discarded — `lot/src/macos/mod.rs:252`
Consistent with cross-platform best-effort design.

### 6.5 [Medium/Error-handling] `clock_gettime` discarded — `lot/src/linux/cgroup.rs:120`
`CLOCK_MONOTONIC` cannot realistically fail on cgroup v2 kernels. Retry loop mitigates collision risk.

### 6.6 [Medium/Error-handling] `close(fd)` discarded — `lot/src/linux/namespace.rs:350`
Standard practice for close-after-successful-open on an empty file.

### 6.7 [Medium/Error-handling] `delete_profile` discarded on spawn error — `lot/src/windows/appcontainer.rs:486`
Primary spawn error is more important. Orphaned profile cleaned up by OS.

---

## Group 7: Documentation & Naming

### 7.1 [Medium/Naming] `kill_by_pid` doc says "signal" on Windows — `lot/src/windows/mod.rs:92`
Windows has no signals; `TerminateProcess` forcibly terminates.

### 7.2 [Medium/Doc-mismatch] `is_strict_parent_of` doc inconsistency — `lot/src/path_util.rs:28-31`
Doc claims consistency with `is_descendant_or_equal`, but they use different canonicalization strategies. For fully-existing paths the behavior converges.

---

## Group 8: Linux Spawn Readability

### 8.1 [Medium/Separation] Deeply nested spawn logic — `lot/src/linux/mod.rs:233-487`
Helper-process and inner-child logic are deeply nested `if` branches. Post-fork code avoids heap allocation (constraining extraction), but named helper functions would improve readability.

---

## Group 9: CI Robustness

### 9.1 [Medium/Correctness] Cgroup path guard missing — `.github/workflows/ci.yml:63-97`
If `grep '^0::'` produces no output, cgroup setup operates on non-cgroup paths. Should guard with `[ -n "$cgroup_path" ]`. Mitigated by GitHub Actions runners always having cgroup v2.

---

## Group 10: Test Helper Return Values

### 10.1 [Medium/Error-handling] `libc::write`/`read` discarded in test helpers — `lot/src/unix.rs:935,949-950,1081,1253-1254,1424-1425`
Failed writes could cause false positives in tests.

### 10.2 [Medium/Error-handling] `write_fd`/`waitpid` discarded in test helpers — `lot/src/linux/mod.rs:593-607,794,842,891,1007`
`write_fd` intentionally discards for async-signal-safety (documented). `waitpid` not checked; failure unrealistic.

### 10.3 [Medium/Error-handling] `waitpid` discarded in seccomp test — `lot/src/linux/seccomp.rs:458`
Test-only; failure unrealistic on valid forked PID.

### 10.4 [Medium/Error-handling] Test results discarded — `lot/src/windows/appcontainer.rs:1196,1243,1293`
`wait_with_output()` and `delete_profile` results discarded. Silent cleanup failure could affect subsequent runs.

---

## Group 11: Test Coverage — Windows

### 11.1 [Medium/Testing] `kill_by_pid` no Windows-specific tests — `lot/src/windows/mod.rs:92-109`
Safety-critical guards (pid==0, pid==self) tested in `lib.rs` under tokio feature but not in isolation.

### 11.2 [Medium/Testing] `acl_helpers` error paths untested — `lot/src/windows/acl_helpers.rs:142-290`
ERROR_ACCESS_DENIED branch producing ELEVATION_REQUIRED_MARKER untested.

### 11.3 [Medium/Testing] `hresult_to_io` untested — `lot/src/windows/appcontainer.rs:86-97`
Branching logic (FACILITY_WIN32 vs fallback) untested.

### 11.4 [Medium/Testing] `cleanup_stale` error paths untested — `lot/src/windows/mod.rs:63-85`
Scan errors, restore failures, deletion failures untested.

### 11.5 [Medium/Testing] Trailing backslash in program path untested — `lot/src/windows/cmdline.rs:243-249`
Produces malformed command line; no test coverage.

---

## Group 12: Test Coverage — macOS

### 12.1 [Medium/Testing] `apply_profile` untested — `lot/src/macos/seatbelt.rs:335-362`
Tested indirectly via integration tests. Direct testing requires forking (sandbox_init is permanent).

### 12.2 [Medium/Testing] `escape_sbpl_path` empty path untested — `lot/src/macos/seatbelt.rs:281-289`
Empty path produces `""` in SBPL. `SandboxPolicy` validates paths before they reach this function.

---

## Group 13: Test Coverage — Linux

### 13.1 [Medium/Testing] Namespace functions untested — `lot/src/linux/namespace.rs:83-89,92-135,154-177,180-188,205-235,469-511`
`setup_user_namespace`, `mount_system_paths`, `mount_policy_paths`, `mount_deny_paths`, `setup_mount_namespace`, `execute_pivot_root` lack unit tests. Require namespace privileges; inherently integration-test territory.

### 13.2 [Medium/Testing] `is_apparmor_restricted` untested — `lot/src/linux/namespace.rs:37-40`
Tested indirectly via `namespace_available_no_panic`.

### 13.3 [Medium/Testing] Ioctl denial untested — `lot/src/linux/seccomp.rs:332-355`
No test verifies denial for unlisted ioctl request numbers. Allowed ioctls tested.

---

## Group 14: Test Coverage — Cross-Platform & Misc

### 14.1 [Medium/Testing] `spawn()` no direct unit test — `lot/src/lib.rs:202-222`
Tested indirectly via platform-specific integration tests. No test for `SandboxError::InvalidPolicy` propagation.

### 14.2 [Medium/Testing] `wait_with_output_timeout` untested — `lot/src/lib.rs:440-480`
Contains non-trivial logic: `spawn_blocking`, `tokio::select!`, panic-resume, `kill_by_pid` on timeout.

### 14.3 [Medium/Testing] `setup_stdio_fds` untested — `lot/src/unix.rs:505-535`
Fd-aliasing edge cases not covered.

### 14.4 [Medium/Testing] `sentinel_dir` builder untested — `lot/src/policy_builder.rs:257-260`
Zero test coverage; regression would go undetected.

### 14.5 [Medium/Testing] `is_strict_parent_of` canonicalization branch untested — `lot/src/path_util.rs:191-222`
Tests use only non-existent paths. `canonicalize_existing_prefix` tested separately.

---

## Group 15: Test `TempDir` Convention

### 15.1 [Medium/Testing] `TempDir::new()` in policy.rs tests — `lot/src/policy.rs:417-419`
Uses system temp instead of `TempDir::new_in("test_tmp/")`. Tests don't spawn sandboxed processes.

### 15.2 [Medium/Testing] `TempDir::new()` in policy_builder.rs tests — `lot/src/policy_builder.rs:354-356`
Same as above.

### 15.3 [Medium/Testing] `TempDir::new()` in path_util.rs tests — `lot/src/path_util.rs:119-151`
Same as above.

---

## Group 16: Review-Cycle Findings

### 16.1 [Medium/Testing] Missing TMP/TMPDIR rejection tests — `lot/src/env_check.rs`
Only `TEMP` rejection is tested in `validate_env_rejects_temp_outside_write_paths`. No test verifies rejection when only `TMP` or only `TMPDIR` is set to an uncovered path. If the validation loop were accidentally changed to only check `TEMP`, no test would catch the regression for `TMP`/`TMPDIR`.

