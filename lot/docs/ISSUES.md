# Known Issues

Issues grouped by co-fixability, ordered by descending impact.

---

## Group 7: Test Coverage Gaps

Zero coverage on public API surfaces, assertion-free tests, untested platforms.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 26 | lot/tests/integration.rs | 1-1201 | No integration tests for: `allow_network`, `ResourceLimits` enforcement, `kill()`, `kill_and_cleanup()`, `try_wait()`, `take_stdout`/`take_stderr`, `SandboxPolicyBuilder` usage. These are public API surfaces with zero coverage. | High |
| 27 | lot/src/unix.rs | — | File has zero unit tests. All functions (wait, kill, pipe management, error pipe protocol) exercised only indirectly through integration tests that can skip on `PrerequisitesNotMet`. | High |
| 28 | lot/tests/integration.rs | 514-525 | macOS branch of `test_cleanup_after_drop` has no assertion. Test cannot fail on macOS. | Medium |
| 29 | lot/tests/integration.rs | 204-1167 | All tests using `try_spawn` silently return on `PrerequisitesNotMet`. No mechanism to detect when entire suite runs zero assertions. Intentional design for cross-platform CI, but means test pass does not guarantee code was exercised. | Medium |
| 30 | lot/tests/integration.rs | 912-957 | `test_symlink_into_deny_path` is Unix-only (`#[cfg(unix)]`). Symlink bypass attack untested on Windows. Windows deny paths use explicit deny ACEs which may resolve symlinks differently. | Medium |
| 45 | lot/src/windows/pipe.rs | tests | `stdio_pipes_close_owned_skips_inherit` is smoke-only (no-panic). Does not verify the inherited handle remains valid after `close_owned`. A post-call check (e.g., `GetFileType`) would prove the handle was not closed. | Medium |
| 46 | lot/src/windows/pipe.rs | tests | No test for `resolve_stdio_output(SandboxStdio::Null)` — the write path of `open_nul_device` is untested. | Medium |

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
