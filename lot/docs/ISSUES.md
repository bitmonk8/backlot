# Known Issues

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

### 9.13 [Doc-Mismatch] path_util.rs doc says "lexical comparison" fallback
- **File:** `lot/src/path_util.rs` line 9
- **Description:** Says "lexical comparison" fallback. Actual fallback is partial canonicalization via `canonicalize_existing_prefix`.

### 9.14 [Doc-Mismatch] unix.rs comment says "on macOS" but applies to all non-Linux Unix
- **File:** `lot/src/unix.rs` line 114
- **Description:** Says "on macOS" — applies to all non-Linux Unix (`#[cfg(not(target_os = "linux"))]`).
