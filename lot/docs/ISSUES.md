# Known Issues

## Group 1: Sandbox Enforcement Correctness

### 1.1 [Correctness] `kill_and_reap` deadlock — SIGKILL never sent
- **File:** `lot/src/unix.rs` lines 653-658
- **Description:** `kill_and_reap` sets `self.waited` to `true` via CAS (line 653-656), then calls `self.kill()` (line 658). But `kill()` checks `waited` and returns early when true. SIGKILL is never sent. The subsequent `waitpid` blocks indefinitely (or until child exits on its own).

### 1.2 [Correctness] `bind_mount_readonly` submounts remain writable
- **File:** `lot/src/linux/namespace.rs` lines 374, 388
- **Description:** Initial bind uses `MS_BIND | MS_REC`, but remount uses `MS_BIND | MS_REMOUNT` without `MS_REC`. On Linux, this only makes the top-level mount read-only — submounts retain original writable flags. Security gap: nested mount points under read-paths remain writable.

### 1.3 [Correctness] `escape_sbpl_path` produces invalid SBPL escapes
- **File:** `lot/src/macos/seatbelt.rs` line 284
- **Description:** `escape_sbpl_path` escapes `)` to `\)`, but SBPL string literals don't define `\)` as a valid escape. The backslash becomes literal, causing rule to match wrong path for paths containing `)`. Also does not escape backslash (`\`), which would be interpreted as escape sequence start.

---

## Group 2: Policy Validation & Path Safety

### 2.1 [Correctness] TEMP and PATH validation ignores deny_paths
- **File:** `lot/src/env_check.rs` lines 55-69, 73-88
- **Description:** TEMP and PATH validation do not account for deny_paths. A TEMP dir under a deny_path that overrides a write_path grant passes validation but fails at runtime. Same issue for PATH entries under deny_paths.

### 2.2 [Correctness] `effective_env` first-match may disagree with spawner semantics
- **File:** `lot/src/env_check.rs` lines 100-113
- **Description:** `effective_env` returns first match in `command.env` Vec. If spawner uses last-wins semantics for duplicate keys, validation checks the wrong directory. Requires user misuse (calling `.env()` with same key twice); matches platform `getenv` first-match behavior.

### 2.3 [Correctness] `collect_validation_error` silently discards non-`InvalidPolicy` errors
- **File:** `lot/src/policy.rs` lines 231-235
- **Description:** `collect_validation_error` silently discards non-`InvalidPolicy` error variants. Currently all callers only produce `InvalidPolicy`, but a defensive catch-all would guard against future changes.

### 2.4 [Correctness] `validate()` runs checks on partially-canonicalized paths
- **File:** `lot/src/policy.rs` lines 273-330
- **Description:** `validate()` runs overlap/coverage checks on partially-canonicalized path sets. Paths that fail canonicalization are omitted from overlap checks, but their errors are still collected. No user-visible incorrect behavior.

### 2.5 [Simplification] Double canonicalization in policy pipeline
- **Files:** `lot/src/policy.rs`, `lot/src/policy_builder.rs`
- **Description:** Builder canonicalizes on insert, `validate()` re-canonicalizes.

### 2.6 [Simplification] `check_cross_overlap` logic duplication
- **File:** `lot/src/policy.rs` lines 86-148
- **Description:** `check_cross_overlap` and `check_cross_overlap_directional` share most logic. Could unify (~30 lines saved).

### 2.7 [Separation] Platform directory lists maintained independently
- **Files:** Platform dirs across `linux/mod.rs`, `macos/mod.rs`, `windows/mod.rs`
- **Description:** Three overlapping platform directory lists maintained independently. Adding a dir to one without updating others could cause validation failures or sandbox escapes.

### 2.8 [Naming] `platform_implicit_read_paths` misnomer
- **File:** `lot/src/linux/mod.rs` line 21
- **Description:** `platform_implicit_read_paths` includes executable dirs (`/bin`, `/usr/bin`). Name says "read" but returns exec paths.

---

## Group 3: CI Test Reliability — Silent Skips

### 3.1 [Testing] AppContainer tests silently skip when non-elevated
- **File:** `lot/src/windows/appcontainer.rs` lines 977-986
- **Description:** `try_spawn` silently returns `None` on `PrerequisitesNotMet`. All spawn-dependent tests could be no-ops in non-elevated CI.

### 3.2 [Testing] Cgroup tests silently skip when unavailable
- **File:** `lot/src/linux/cgroup.rs` lines 303-420
- **Description:** All tests silently skip when cgroups unavailable. CI can report 100% pass with 0% execution.

### 3.3 [Testing] Prerequisites test is tautological
- **File:** `lot/src/windows/prerequisites.rs` lines 69-107
- **Description:** Tautological test (mirrors production logic) and three early-return skip paths. Test cannot fail independently of the code it tests.

---

## Group 4: Linux Cgroup Issues

### 4.1 [Correctness] Post-fork child calls `std::process::exit` instead of `libc::_exit`
- **File:** `lot/src/linux/cgroup.rs` line 351, 401
- **Description:** `std::process::exit(0)` in forked child should be `libc::_exit(0)`. Running Rust destructors in post-fork child is unsafe per POSIX. Practically unreachable (after `libc::pause()`), but the fix is trivial.

### 4.2 [Correctness] Test leaves zombie process
- **File:** `lot/src/linux/cgroup.rs` lines 346-352
- **Description:** Test `cgroup_guard_add_process` does not reap forked child, leaving zombie until test process exits. Inconsistent with the `ChildGuard` pattern used in other tests in the same file.

### 4.3 [Simplification] Near-identical cgroup blocks and duplicated cgroup.kill
- **File:** `lot/src/linux/cgroup.rs` lines 34-253
- **Description:** Near-identical check blocks, duplicated `cgroup.kill` in drop vs `kill_all`.

### 4.4 [Cruft] `#[allow(dead_code)]` references non-existent fork path
- **File:** `lot/src/linux/cgroup.rs` line 175
- **Description:** `#[allow(dead_code)]` comment references non-existent "fork path" caller. Method only called from tests.

---

## Group 5: Unix Process Management & Testing

### 5.1 [Testing] `unix.rs` has zero unit tests
- **File:** `lot/src/unix.rs` lines 1-672
- **Description:** Entire file has zero unit tests. Pure functions `prepare_prefork`, `read_two_fds`, `check_child_error_pipe`, `exit_status_from_raw` are directly testable but untested.

### 5.2 [Simplification] `unix.rs` unnecessary intermediate Vec and dead error handler
- **File:** `lot/src/unix.rs` lines 47-103
- **Description:** Unnecessary intermediate Vec, match instead of map, dead error handler.

### 5.3 [Correctness] Integration test `try_wait` loop has no iteration bound
- **File:** `lot/tests/integration.rs` lines 1222-1234
- **Description:** `test_try_wait_returns_none_then_some` loop has no iteration bound. Could hang in CI if `kill()` fails to terminate the process.

---

## Group 6: Windows ACL & Error Handling

### 6.1 [Correctness] Hardcoded `ACL_REVISION` breaks non-standard DACLs
- **File:** `lot/src/windows/traverse_acl.rs` lines 203, 229, 248
- **Description:** New ACL hardcodes `ACL_REVISION` (2). If original DACL uses `ACL_REVISION_DS` (4), `AddAce` will fail. Not applicable to file system DACLs in practice, but would break on non-standard DACL configurations.

### 6.2 [Separation] Two parallel DACL-writing codepaths
- **Files:** `lot/src/windows/traverse_acl.rs`, `lot/src/windows/acl_helpers.rs`
- **Description:** Two parallel DACL-writing codepaths with different semantics.

### 6.3 [Error-Handling] `cleanup` errors not propagated
- **File:** `lot/src/windows/appcontainer.rs` lines 412-421
- **Description:** `cleanup` prints errors to stderr but never propagates them. `kill_and_cleanup` returns `Ok(())`. Errors are logged, and `cleanup()` is also called from `Drop` where propagation is impossible. Defensible design, but callers of `kill_and_cleanup` cannot detect cleanup failures.

### 6.4 [Error-Handling] Sentinel silently skips unreadable directory entries
- **File:** `lot/src/windows/sentinel.rs` lines 252-253
- **Description:** `find_stale_sentinels_in` silently skips directory entries that fail to read. Reasonable for a best-effort cleanup scan.

### 6.5 [Error-Handling] Sentinel silently discards non-NotFound parse errors
- **File:** `lot/src/windows/sentinel.rs` lines 280-283
- **Description:** `find_stale_sentinels_in` silently discards non-NotFound sentinel parse errors. Prevents one corrupted file from blocking cleanup of others.

---

## Group 7: Cross-Platform Code Deduplication

### 7.1 [Simplification] Three-layer delegation wrappers
- **Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`, `lot/src/unix.rs`, `lot/src/lib.rs`
- **Description:** Three layers of delegation wrappers (`SandboxedChild` -> platform wrapper -> `UnixSandboxedChild`). ~100 lines of boilerplate.

### 7.2 [Simplification] Duplicated dup2 and setrlimit sequences
- **Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`
- **Description:** Identical 18-line dup2 sequences duplicated across Linux and macOS spawn paths. Repeated `setrlimit` calls in macOS also duplicate Linux pattern. Could extract shared helpers.

### 7.3 [Simplification] Triplicated `kill_by_pid` guard logic
- **Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`, `lot/src/windows/mod.rs`
- **Description:** Triplicated `kill_by_pid` guard logic.

---

## Group 8: Platform-Specific Test Gaps

### 8.1 [Testing] `close_inherited_fds` untested
- **File:** `lot/src/linux/mod.rs` lines 104-146
- **Description:** `close_inherited_fds` has no unit tests despite non-trivial sort/dedup/gap-computation logic.

### 8.2 [Testing] Seccomp argument-filtered rules untested
- **File:** `lot/src/linux/seccomp.rs` lines 206-256
- **Description:** Argument-filtered prctl/ioctl rules have no integration tests. Most complex filter logic in the seccomp module.

### 8.3 [Testing] macOS spawn error paths untested
- **File:** `lot/src/macos/mod.rs` lines 91-278
- **Description:** `spawn` error paths untested: nonexistent program, invalid cwd, seatbelt rejection.

### 8.4 [Testing] `forward_common_env` Windows case-insensitive dedup untested
- **File:** `lot/src/command.rs` lines 111-153
- **Description:** `forward_common_env` case-insensitive dedup untested on Windows. Could cause duplicate env vars.

### 8.5 [Testing] Job limit branches untested
- **File:** `lot/src/windows/job.rs` lines 41-136
- **Description:** No test exercises individual limit branches. No readback verification. A typo in a limit constant would go undetected.

### 8.6 [Testing] `wait_with_output_timeout` panic/JoinError paths untested
- **File:** `lot/src/lib.rs` lines 428-464
- **Description:** `wait_with_output_timeout` panic-resume and JoinError paths untested. Represents real failure modes in production.

### 8.7 [Simplification] Seccomp duplicated prctl/ioctl blocks and test scaffolding
- **File:** `lot/src/linux/seccomp.rs` lines 208-598
- **Description:** Duplicate prctl/ioctl blocks, duplicated fork scaffolding in tests.

---

## Group 9: Documentation Accuracy

### 9.1 [Doc-Mismatch] DESIGN.md claims cpu.max is enforced
- **File:** `docs/DESIGN.md` line 91
- **Description:** Lists cpu.max as enforced. Code does NOT write cpu.max (intentionally not enforced via cgroups).

### 9.2 [Doc-Mismatch] DESIGN.md claims spawn checks for stale sentinels
- **File:** `docs/DESIGN.md` line 175
- **Description:** Says spawn checks for stale sentinels. Requires explicit `cleanup_stale()` call.

### 9.3 [Doc-Mismatch] policy.rs doc says paths must not overlap
- **File:** `lot/src/policy.rs` lines 8-10
- **Description:** Says "Grant paths must not overlap." Code allows write children under read parents (intentional directional overlap).

### 9.4 [Doc-Mismatch] README says empty config is valid
- **Files:** `README.md`, `lot/src/policy.rs`, `lot-cli/src/config.rs`
- **Description:** README says empty config is valid. Code rejects empty policies via `validate()`.

### 9.5 [Doc-Mismatch] DESIGN.md omits partial canonicalization
- **File:** `docs/DESIGN.md` line 17
- **Description:** Omits partial/best-effort canonicalization for path_util.rs. Module provides `canonicalize_existing_prefix` and `canonicalize_best_effort`.

### 9.6 [Doc-Mismatch] DESIGN.md misattributes fork/unshare to namespace.rs
- **File:** `docs/DESIGN.md` line 21
- **Description:** Says namespace.rs handles fork()+unshare() — those calls are in mod.rs.

### 9.7 [Doc-Mismatch] DESIGN.md omits MS_NOEXEC from mount flags
- **File:** `docs/DESIGN.md` line 97
- **Description:** Omits `MS_NOEXEC` from read-only mount flags. Code applies `MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC`.

### 9.8 [Doc-Mismatch] DESIGN.md oversimplifies Windows deny-path mechanism
- **File:** `docs/DESIGN.md` line 159
- **Description:** Oversimplifies Windows deny-path mechanism. Omits the DACL protection step (`PROTECTED_DACL_SECURITY_INFORMATION`) that makes deny paths effective.

### 9.9 [Doc-Mismatch] DESIGN.md overstates cgroup unavailability error
- **File:** `docs/DESIGN.md` line 255
- **Description:** Says cgroup unavailable always errors. Code only errors when resource limits are actually requested.

### 9.10 [Doc-Mismatch] env_check.rs doc implies single accessibility rule
- **File:** `lot/src/env_check.rs` lines 24-30
- **Description:** Function doc implies single accessibility rule. TEMP uses a stricter write-path-only check, while PATH uses grant paths + implicit paths.

### 9.11 [Doc-Mismatch] macOS `kill_and_cleanup` doc describes wrong operation order
- **File:** `lot/src/macos/mod.rs` lines 321-328
- **Description:** `kill_and_cleanup` doc says kill/wait/close. Code does close/kill/wait.

### 9.12 [Doc-Mismatch] seatbelt escape doc incomplete
- **File:** `lot/src/macos/seatbelt.rs` lines 274-275
- **Description:** Doc says only `"` is escaped. Code also escapes `)`.

### 9.13 [Doc-Mismatch] path_util.rs doc says "lexical comparison" fallback
- **File:** `lot/src/path_util.rs` line 9
- **Description:** Says "lexical comparison" fallback. Actual fallback is partial canonicalization via `canonicalize_existing_prefix`.

### 9.14 [Doc-Mismatch] unix.rs comment says "on macOS" but applies to all non-Linux Unix
- **File:** `lot/src/unix.rs` line 114
- **Description:** Says "on macOS" — applies to all non-Linux Unix (`#[cfg(not(target_os = "linux"))]`).
