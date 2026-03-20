# Audit Findings (Verified & Filtered)

## Summary

| Severity | Count |
|----------|-------|
| Critical | 2 |
| High | 12 |
| Medium | 27 |
| **Total** | **41** |

| Category | Critical | High | Medium | Total |
|----------|----------|------|--------|-------|
| Correctness | 2 | 5 | 9 | 16 |
| Error Handling | 0 | 5 | 7 | 12 |
| Testing | 0 | 2 | 3 | 5 |
| Doc Mismatch | 0 | 2 | 4 | 6 |
| Separation | 0 | 0 | 2 | 2 |

Removed from original 180 findings: all Low severity (113), all Naming (subjective), all Simplification (subjective), duplicates, one incorrect finding (acl_helpers.rs null SD "dangling pointer" — mechanism was wrong). Several severities adjusted after code verification.

Key themes:
- **Security-relevant silent failures**: SBPL rules silently dropped for bad paths (macOS), sentinel entries silently skipped (Windows)
- **Windows handle management**: Console handle corruption on error paths, stdio Null producing INVALID_HANDLE_VALUE instead of NUL device
- **Error swallowing**: Sentinel restoration errors discarded, traverse ACL control flags silently lost
- **Test coverage gaps**: Zero unit tests for unix.rs, no integration tests for network/resource-limit/kill APIs
- **DESIGN.md drift**: Device mount flags, macOS surface area, clone vs unshare, process model inaccuracies

---

## Correctness

### File: lot/src/macos/seatbelt.rs
- **Line(s):** 288-308
- **Description:** `append_sbpl_rule` silently drops rules when `escape_sbpl_path` fails. For deny paths, the sandboxed process retains access to a path the policy intended to block. Security-relevant silent failure.
- **Severity:** Critical

### File: lot/src/windows/appcontainer.rs
- **Line(s):** 607-608, 617, 669, 699, 726
- **Description:** On error paths only, `close_handle_if_valid(child_stdin/stdout/stderr)` closes parent's own console handle when stdio is `Inherit`. `GetStdHandle()` returns a borrowed console handle; closing it corrupts parent's console I/O. Success path correctly avoids this.
- **Severity:** Critical

- **Line(s):** 797-805
- **Description:** When `STARTF_USESTDHANDLES` is set and a stream is `Null`, `INVALID_HANDLE_VALUE` is passed as the child's handle — not a handle to NUL device. Child receives an invalid handle. Only manifests when mixing `Null` with `Piped`/`Inherit`.
- **Severity:** High

### Cross-file: lot-cli/src/main.rs, lot/src/command.rs, lot/src/env_check.rs
- **Description:** CLI calls `forward_common_env()` before explicit user vars. Since consumers take first match, user's explicit env overrides are silently ignored. CLI-only issue; library callers control call order.
- **Severity:** High
- **Note:** Downgraded from Critical. CLI-only, easy fix (swap call order).

### File: lot/src/unix.rs
- **Line(s):** 396-423
- **Description:** `check_child_error_pipe` only handles exactly 8 bytes or 0. Short reads (1-7 bytes) or negative returns (non-EINTR errors) fall through to `Ok(())`, treating a child error report as success. Note: short reads on 8-byte pipe writes are atomic on all Unix systems (within PIPE_BUF), so this is unlikely in practice.
- **Severity:** High

- **Line(s):** 509-521
- **Description:** `try_wait` sets `waited = true` before calling `waitpid(WNOHANG)`, then reverts on rc==0. Between CAS and revert, concurrent callers see `waited == true` and are spuriously rejected.
- **Severity:** Medium

- **Line(s):** 127-142
- **Description:** On macOS `make_pipe` path, if `fcntl(F_GETFD)` returns -1, code silently skips `O_CLOEXEC`. If `fcntl(F_SETFD)` fails, return value is ignored. Pipe fd could leak to child.
- **Severity:** Medium

- **Line(s):** 587-603
- **Description:** `kill_and_reap` reads `waited` flag without CAS, unlike `wait()` which uses `compare_exchange`. Theoretical TOCTOU if called concurrently.
- **Severity:** Medium
- **Note:** Downgraded from High. `kill_and_reap` takes `&mut self`, preventing concurrent safe calls.

### File: lot/src/linux/mod.rs
- **Line(s):** 361
- **Description:** `prctl(PR_SET_PDEATHSIG, SIGKILL)` race: if helper dies between `fork()` and `prctl()`, inner child is reparented to init and death signal never delivered. Standard fix: check `getppid()` after `prctl`. Window is extremely small in practice.
- **Severity:** High

### File: lot/src/linux/namespace.rs
- **Line(s):** 139-158
- **Description:** `mount_policy_paths` unconditionally uses `mkdir_p` + directory bind-mount for all policy paths. If a policy path refers to a regular file, `mkdir_p` creates a directory and the bind mount fails. No file-vs-directory detection.
- **Severity:** High

### File: lot/src/linux/cgroup.rs
- **Line(s):** 232-266
- **Description:** If `cgroup.kill` succeeds but processes don't exit within ~100ms poll budget, `remove_dir` is called while processes may still be alive, causing EBUSY. The `remove_dir` error is silently ignored, leaking the cgroup.
- **Severity:** Medium

- **Line(s):** 55-62
- **Description:** Fallback `is_writable(&cgroup_path)` in `has_writable_subtree` doesn't check `subtree_control` controllers. `available()` can return true when cgroup creation will actually fail.
- **Severity:** Medium

### File: lot/src/path_util.rs
- **Line(s):** 65-67
- **Description:** `normalize_lexical` can silently collapse excess `..` to root. `debug_assert` disabled in release. Potential path-traversal issue if untrusted path reaches this function.
- **Severity:** Medium

### File: lot/src/policy_builder.rs
- **Line(s):** 59-68, 88-96, 70-83
- **Description:** Builder doc promises "overlap deduction" but doesn't deduplicate across read/exec or write/exec boundaries. Overlapping cross-set paths pass builder but fail at `validate()`.
- **Severity:** Medium

### File: lot/src/windows/sentinel.rs
- **Line(s):** 83
- **Description:** If `sd_to_sddl()` returns error, `?` causes early return skipping `LocalFree(sd)`. Security descriptor memory leaked on error path.
- **Severity:** Medium

- **Line(s):** 47-58
- **Description:** `is_process_alive` susceptible to PID reuse. Stale sentinel may be skipped if PID has been reused by another process, leaving ACL changes permanent.
- **Severity:** Medium

---

## Error Handling

### File: lot/src/windows/appcontainer.rs
- **Line(s):** 410
- **Description:** `restore_from_sentinel` errors silently discarded in `cleanup()`. Filesystem left in modified security state.
- **Severity:** High

- **Line(s):** 524
- **Description:** `restore_from_sentinel` error silently discarded during spawn error cleanup. ACL restoration failure lost. Unlike drop context, this error could be propagated.
- **Severity:** High

### File: lot/src/windows/sentinel.rs
- **Line(s):** 293-298
- **Description:** Malformed sentinel entries silently skipped. ACLs for those paths are never restored.
- **Severity:** High

- **Line(s):** 233
- **Description:** `to_string_lossy` replaces non-Unicode paths with U+FFFD. Restoration targets wrong path. Rare on Windows in practice (paths are natively UTF-16).
- **Severity:** High

- **Line(s):** 362-364
- **Description:** `cleanup_stale` returns `Ok(())` when `read_dir` fails. Caller cannot distinguish "no stale sentinels" from "couldn't read sentinel directory."
- **Severity:** Medium

### File: lot/src/windows/traverse_acl.rs
- **Line(s):** 319-331
- **Description:** `GetSecurityDescriptorControl` and `SetSecurityDescriptorControl` failures silently ignored. Can produce broken DACL state (missing SE_DACL_AUTO_INHERITED flag).
- **Severity:** High

- **Line(s):** 90-96
- **Description:** `has_traverse_ace` returns `false` when `read_dacl` fails. Cannot distinguish "no ACE present" from "query failed."
- **Severity:** Medium

### File: lot/src/windows/acl_helpers.rs
- **Line(s):** 103, 294-317
- **Description:** `allocate_app_packages_sid` and `read_dacl` return `Option` discarding Win32 error codes. Callers cannot diagnose failures.
- **Severity:** Medium

- **Line(s):** 330-352
- **Description:** `dacl_has_app_packages_ace` returns `false` on SID allocation or ACL query failure. Cannot distinguish "no ACE" from "query failed."
- **Severity:** Medium

### File: lot/src/macos/mod.rs
- **Line(s):** 106
- **Description:** `canonicalize` failure silently swallowed. Delays error to child execve, producing a less actionable error message.
- **Severity:** Medium

### File: lot/src/linux/cgroup.rs
- **Line(s):** 144-152
- **Description:** If limit write fails after cgroup directory creation, cgroup directory is leaked (not cleaned up on error path).
- **Severity:** Medium

- **Line(s):** 188-189
- **Description:** `kill_all` silently returns on `cgroup.procs` read failure. Processes in the cgroup may survive.
- **Severity:** Medium

---

## Testing

### File: lot/tests/integration.rs
- **Line(s):** 1-1139
- **Description:** No integration tests for: `allow_network`, `ResourceLimits` enforcement, `kill()`, `kill_and_cleanup()`, `try_wait()`, `take_stdout`/`take_stderr`, `SandboxPolicyBuilder` usage. These are public API surfaces with zero coverage.
- **Severity:** High

- **Line(s):** 452-463
- **Description:** macOS branch of `test_cleanup_after_drop` has no assertion. Test cannot fail on macOS.
- **Severity:** Medium

### File: lot/src/unix.rs
- **Line(s):** 1-605
- **Description:** File has zero unit tests. All functions (wait, kill, pipe management, error pipe protocol) exercised only indirectly through integration tests that can skip on `PrerequisitesNotMet`.
- **Severity:** High

### File: lot/tests/integration.rs
- **Line(s):** 204-1105
- **Description:** All tests using `try_spawn` silently return on `PrerequisitesNotMet`. No mechanism to detect when entire suite runs zero assertions. Intentional design for cross-platform CI, but means test pass does not guarantee code was exercised.
- **Severity:** Medium
- **Note:** Downgraded from High. Pattern is intentional and documented in DESIGN.md; diagnostic messages are printed on skip.

- **Line(s):** 850-895
- **Description:** `test_symlink_into_deny_path` is Unix-only (`#[cfg(unix)]`). Symlink bypass attack untested on Windows. Windows deny paths use explicit deny ACEs which may resolve symlinks differently.
- **Severity:** Medium
- **Note:** Downgraded from High. Windows ACE evaluation operates on resolved paths, reducing (but not eliminating) symlink bypass risk.

---

## Doc Mismatch

### File: docs/DESIGN.md
- **Line(s):** 97
- **Description:** Omits `/dev/zero` from device list. Claims devices are "read-only" but they are bind-mounted without `MS_RDONLY`. Device nodes are writable when documentation says they should be read-only.
- **Severity:** High

- **Line(s):** 119
- **Description:** Substantially understates macOS always-allowed surface area. Lists only `/usr/lib`, `/System/Library`, dynamic linker cache, `/dev/urandom`. Actual code allows `/System/Cryptexes`, `/Library/Preferences`, `/Library/Apple`, `/dev/random`, `/dev/null`, `/dev/fd`, 8 system exec paths, 15 metadata paths, plus `process-fork`, `sysctl-read`, `iokit-open`, `mach-lookup`, and more.
- **Severity:** High

- **Line(s):** 19, 99
- **Description:** Says Linux uses `clone()` with namespace flags. Implementation uses `fork()` + `unshare()`. No `clone()` syscall exists in the codebase.
- **Severity:** Medium

- **Line(s):** 122
- **Description:** Says "Fork a helper" for macOS. macOS forks child directly (single fork + setsid + sandbox_init + execve). The "helper" pattern is Linux-only.
- **Severity:** Medium

- **Line(s):** 124
- **Description:** Says macOS resource limits are only `RLIMIT_AS`. Code also applies `RLIMIT_NPROC` and `RLIMIT_CPU`.
- **Severity:** Medium

- **Line(s):** 5-45
- **Description:** Project structure tree omits 4 source files: `env_check.rs`, `path_util.rs`, `windows/prerequisites.rs`, `lot-cli/src/config.rs`.
- **Severity:** Medium

---

## Separation

### Cross-file: lot/src/env_check.rs, lot/src/unix.rs
- **Description:** Default Unix PATH defined in two places (`DEFAULT_UNIX_PATH` const and inline byte literal in `build_envp`). If one updated without the other, validation checks a different PATH than what gets injected at runtime.
- **Severity:** Medium
- **Note:** Downgraded from High. DRY violation with correctness implications, but not a current bug.

### Cross-file: lot/src/policy.rs, lot/src/policy_builder.rs
- **Description:** Path canonicalization and overlap validation logic exists in both files with different mechanisms. Changes to validation rules require updates in two places.
- **Severity:** Medium
