# Known Issues

Issues grouped by co-fixability, ordered by descending impact.

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
