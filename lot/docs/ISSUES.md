# Known Issues

Issues grouped by co-fixability, ordered by descending impact.

---

## Group 2: Windows Process Creation & Stdio Handle Safety

Console handle corruption on error paths and invalid handles passed to child processes.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 3 | lot/src/windows/appcontainer.rs | 607-608, 617, 669, 699, 726 | On error paths only, `close_handle_if_valid(child_stdin/stdout/stderr)` closes parent's own console handle when stdio is `Inherit`. `GetStdHandle()` returns a borrowed console handle; closing it corrupts parent's console I/O. Success path correctly avoids this. | Critical |
| 4 | lot/src/windows/appcontainer.rs | 797-805 | When `STARTF_USESTDHANDLES` is set and a stream is `Null`, `INVALID_HANDLE_VALUE` is passed as the child's handle — not a handle to NUL device. Child receives an invalid handle. Only manifests when mixing `Null` with `Piped`/`Inherit`. | High |

---

## Group 3: Windows Sentinel & ACL Recovery Robustness

Sentinel errors discarded, malformed entries skipped, memory leaks. Security state left permanently modified after failures.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 5 | lot/src/windows/appcontainer.rs | 410 | `restore_from_sentinel` errors silently discarded in `cleanup()`. Filesystem left in modified security state. | High |
| 6 | lot/src/windows/appcontainer.rs | 524 | `restore_from_sentinel` error silently discarded during spawn error cleanup. ACL restoration failure lost. Unlike drop context, this error could be propagated. | High |
| 7 | lot/src/windows/sentinel.rs | 293-298 | Malformed sentinel entries silently skipped. ACLs for those paths are never restored. | High |
| 8 | lot/src/windows/sentinel.rs | 233 | `to_string_lossy` replaces non-Unicode paths with U+FFFD. Restoration targets wrong path. Rare on Windows in practice (paths are natively UTF-16). | High |
| 9 | lot/src/windows/sentinel.rs | 83 | If `sd_to_sddl()` returns error, `?` causes early return skipping `LocalFree(sd)`. Security descriptor memory leaked on error path. | Medium |
| 10 | lot/src/windows/sentinel.rs | 47-58 | `is_process_alive` susceptible to PID reuse. Stale sentinel may be skipped if PID has been reused by another process, leaving ACL changes permanent. | Medium |
| 11 | lot/src/windows/sentinel.rs | 362-364 | `cleanup_stale` returns `Ok(())` when `read_dir` fails. Caller cannot distinguish "no stale sentinels" from "couldn't read sentinel directory." | Medium |

---

## Group 4: Unix Process Lifecycle Safety

Error pipe misreporting, race conditions in wait/kill, orphaned child processes.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 12 | lot/src/unix.rs | 396-423 | `check_child_error_pipe` only handles exactly 8 bytes or 0. Short reads (1-7 bytes) or negative returns (non-EINTR errors) fall through to `Ok(())`, treating a child error report as success. Note: short reads on 8-byte pipe writes are atomic on all Unix systems (within PIPE_BUF), so this is unlikely in practice. | High |
| 13 | lot/src/linux/mod.rs | 361 | `prctl(PR_SET_PDEATHSIG, SIGKILL)` race: if helper dies between `fork()` and `prctl()`, inner child is reparented to init and death signal never delivered. Standard fix: check `getppid()` after `prctl`. Window is extremely small in practice. | High |
| 14 | lot/src/unix.rs | 509-521 | `try_wait` sets `waited = true` before calling `waitpid(WNOHANG)`, then reverts on rc==0. Between CAS and revert, concurrent callers see `waited == true` and are spuriously rejected. | Medium |
| 15 | lot/src/unix.rs | 587-603 | `kill_and_reap` reads `waited` flag without CAS, unlike `wait()` which uses `compare_exchange`. Theoretical TOCTOU if called concurrently. Downgraded: `kill_and_reap` takes `&mut self`, preventing concurrent safe calls. | Medium |
| 16 | lot/src/unix.rs | 127-142 | On macOS `make_pipe` path, if `fcntl(F_GETFD)` returns -1, code silently skips `O_CLOEXEC`. If `fcntl(F_SETFD)` fails, return value is ignored. Pipe fd could leak to child. | Medium |

---

## Group 5: Windows ACL/DACL Error Propagation

Error codes discarded, failure indistinguishable from "not present". Broken DACL state possible.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 17 | lot/src/windows/traverse_acl.rs | 319-331 | `GetSecurityDescriptorControl` and `SetSecurityDescriptorControl` failures silently ignored. Can produce broken DACL state (missing SE_DACL_AUTO_INHERITED flag). | High |
| 18 | lot/src/windows/traverse_acl.rs | 90-96 | `has_traverse_ace` returns `false` when `read_dacl` fails. Cannot distinguish "no ACE present" from "query failed." | Medium |
| 19 | lot/src/windows/acl_helpers.rs | 103, 294-317 | `allocate_app_packages_sid` and `read_dacl` return `Option` discarding Win32 error codes. Callers cannot diagnose failures. | Medium |
| 20 | lot/src/windows/acl_helpers.rs | 330-352 | `dacl_has_app_packages_ace` returns `false` on SID allocation or ACL query failure. Cannot distinguish "no ACE" from "query failed." | Medium |

---

## Group 6: Linux Cgroup & Mount Robustness

File bind-mount failure, cgroup leaks, silent process survival.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 21 | lot/src/linux/namespace.rs | 139-158 | `mount_policy_paths` unconditionally uses `mkdir_p` + directory bind-mount for all policy paths. If a policy path refers to a regular file, `mkdir_p` creates a directory and the bind mount fails. No file-vs-directory detection. | High |
| 22 | lot/src/linux/cgroup.rs | 232-266 | If `cgroup.kill` succeeds but processes don't exit within ~100ms poll budget, `remove_dir` is called while processes may still be alive, causing EBUSY. The `remove_dir` error is silently ignored, leaking the cgroup. | Medium |
| 23 | lot/src/linux/cgroup.rs | 55-62 | Fallback `is_writable(&cgroup_path)` in `has_writable_subtree` doesn't check `subtree_control` controllers. `available()` can return true when cgroup creation will actually fail. | Medium |
| 24 | lot/src/linux/cgroup.rs | 144-152 | If limit write fails after cgroup directory creation, cgroup directory is leaked (not cleaned up on error path). | Medium |
| 25 | lot/src/linux/cgroup.rs | 188-189 | `kill_all` silently returns on `cgroup.procs` read failure. Processes in the cgroup may survive. | Medium |

---

## Group 7: Test Coverage Gaps

Zero coverage on public API surfaces, assertion-free tests, untested platforms.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 26 | lot/tests/integration.rs | 1-1139 | No integration tests for: `allow_network`, `ResourceLimits` enforcement, `kill()`, `kill_and_cleanup()`, `try_wait()`, `take_stdout`/`take_stderr`, `SandboxPolicyBuilder` usage. These are public API surfaces with zero coverage. | High |
| 27 | lot/src/unix.rs | 1-605 | File has zero unit tests. All functions (wait, kill, pipe management, error pipe protocol) exercised only indirectly through integration tests that can skip on `PrerequisitesNotMet`. | High |
| 28 | lot/tests/integration.rs | 452-463 | macOS branch of `test_cleanup_after_drop` has no assertion. Test cannot fail on macOS. | Medium |
| 29 | lot/tests/integration.rs | 204-1105 | All tests using `try_spawn` silently return on `PrerequisitesNotMet`. No mechanism to detect when entire suite runs zero assertions. Intentional design for cross-platform CI, but means test pass does not guarantee code was exercised. | Medium |
| 30 | lot/tests/integration.rs | 850-895 | `test_symlink_into_deny_path` is Unix-only (`#[cfg(unix)]`). Symlink bypass attack untested on Windows. Windows deny paths use explicit deny ACEs which may resolve symlinks differently. | Medium |

---

## Group 8: CLI Environment Variable Ordering

User's explicit env overrides silently ignored.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 31 | lot-cli/src/main.rs, lot/src/command.rs, lot/src/env_check.rs | — | CLI calls `forward_common_env()` before explicit user vars. Since consumers take first match, user's explicit env overrides are silently ignored. CLI-only issue; library callers control call order. Downgraded from Critical. | High |

---

## Group 9: DESIGN.md Documentation Accuracy

Implementation details diverged from design document.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 32 | docs/DESIGN.md | 97 | Omits `/dev/zero` from device list. Claims devices are "read-only" but they are bind-mounted without `MS_RDONLY`. Device nodes are writable when documentation says they should be read-only. | High |
| 33 | docs/DESIGN.md | 119 | Substantially understates macOS always-allowed surface area. Lists only `/usr/lib`, `/System/Library`, dynamic linker cache, `/dev/urandom`. Actual code allows `/System/Cryptexes`, `/Library/Preferences`, `/Library/Apple`, `/dev/random`, `/dev/null`, `/dev/fd`, 8 system exec paths, 15 metadata paths, plus `process-fork`, `sysctl-read`, `iokit-open`, `mach-lookup`, and more. | High |
| 34 | docs/DESIGN.md | 19, 99 | Says Linux uses `clone()` with namespace flags. Implementation uses `fork()` + `unshare()`. No `clone()` syscall exists in the codebase. | Medium |
| 35 | docs/DESIGN.md | 122 | Says "Fork a helper" for macOS. macOS forks child directly (single fork + setsid + sandbox_init + execve). The "helper" pattern is Linux-only. | Medium |
| 36 | docs/DESIGN.md | 124 | Says macOS resource limits are only `RLIMIT_AS`. Code also applies `RLIMIT_NPROC` and `RLIMIT_CPU`. | Medium |
| 37 | docs/DESIGN.md | 5-45 | Project structure tree omits 4 source files: `env_check.rs`, `path_util.rs`, `windows/prerequisites.rs`, `lot-cli/src/config.rs`. | Medium |

---

## Group 10: Policy Validation & Path Safety

Path traversal via lexical normalization, incomplete overlap deduction.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 38 | lot/src/path_util.rs | 65-67 | `normalize_lexical` can silently collapse excess `..` to root. `debug_assert` disabled in release. Potential path-traversal issue if untrusted path reaches this function. | Medium |
| 39 | lot/src/policy_builder.rs | 59-68, 88-96, 70-83 | Builder doc promises "overlap deduction" but doesn't deduplicate across read/exec or write/exec boundaries. Overlapping cross-set paths pass builder but fail at `validate()`. | Medium |

---

## Group 11: Separation of Concerns

Duplicated constants and split validation logic requiring synchronized updates.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 40 | lot/src/env_check.rs, lot/src/unix.rs | — | Default Unix PATH defined in two places (`DEFAULT_UNIX_PATH` const and inline byte literal in `build_envp`). If one updated without the other, validation checks a different PATH than what gets injected at runtime. | Medium |
| 41 | lot/src/policy.rs, lot/src/policy_builder.rs | — | Path canonicalization and overlap validation logic exists in both files with different mechanisms. Changes to validation rules require updates in two places. | Medium |

---

## Group 12: macOS Seatbelt Test Precision

Test improvements deferred from Group 1 fix.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 42 | lot/src/macos/seatbelt.rs | tests | Non-UTF-8 error tests (`generate_profile_errors_on_non_utf8_read_path`, `_deny_path`) only check `msg.contains("not valid UTF-8")`. Should verify error variant and which path triggered the error. | Medium |
| 43 | lot/src/macos/seatbelt.rs | tests | Missing non-UTF-8 test coverage for write_paths and exec_paths vectors. Only read and deny paths are tested. | Medium |
| 44 | lot/src/macos/seatbelt.rs | tests | The two non-UTF-8 test functions share nearly identical bodies. Could be consolidated with a parameterized helper. | Low |
