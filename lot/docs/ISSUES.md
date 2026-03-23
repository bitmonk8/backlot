# Issues

*Imported from audit findings — 2026-03-23.*

---

## Group 3: Linux namespace robustness & deduplication

### 3.2 `mount_tmpfs` / `mount_empty_tmpfs` duplication
- **File:** lot/src/linux/namespace.rs — lines 319-362
- Both functions share identical structure. A single helper with parameters would eliminate duplication.

---

## Group 4: Windows appcontainer simplification

### 4.1 `CreateAppContainerProfile` call duplicated in retry logic
- **File:** lot/src/windows/appcontainer.rs — lines 109-141

### 4.2 `wait()` / `try_wait()` share identical exit-code retrieval
- **File:** lot/src/windows/appcontainer.rs — lines 309-348
- Extract `get_exit_code()` helper.

### 4.3 Stdio pipe error cleanup uses growing cleanup blocks
- **File:** lot/src/windows/appcontainer.rs — lines 655-685
- Guard/scopeguard pattern would simplify.

---

## Group 5: Windows helper deduplication

### 5.1 `modify_dacl` / `read_dacl` duplicate `GetNamedSecurityInfoW` pattern
- **File:** lot/src/windows/acl_helpers.rs — lines 142-179, 297-327
- `modify_dacl` could call `read_dacl` internally.

### 5.2 `resolve_stdio_input` / `resolve_stdio_output` duplicate logic
- **File:** lot/src/windows/pipe.rs — lines 102-159
- Same Null/Piped/Inherit arms. Could be a single function with `is_input` flag.

---

## Group 6: Unix simplification

### 6.1 Dead `CString::new("/dev/null")` error mapping
- **File:** lot/src/unix.rs — lines 98-99
- `CString::new("/dev/null")` can never fail (no NUL in literal). `.map_err(...)` is dead code.

### 6.2 Repeated setrlimit pattern in `apply_resource_limits`
- **File:** lot/src/unix.rs — lines 573-608
- Same pattern repeated 3 times. Extract helper.

---

## Group 7: Miscellaneous simplification

### 7.1 Double `canonicalize_existing_prefix` call in TEMP validation
- **File:** lot/src/env_check.rs — lines 89-121
- Loop calls `canonicalize_existing_prefix` twice on the same path — once inside `is_dir_accessible` and once for diagnostics.

### 7.2 Seccomp `#[cfg(target_arch = "x86_64")]` block consolidation
- **File:** lot/src/linux/seccomp.rs — lines 114-331
- 11 separate `#[cfg(target_arch = "x86_64")]` blocks each calling `allow_syscalls`. Could consolidate into one block.

---

## Group 8: macOS seatbelt comment

### 8.1 Misleading SBPL semantics comment
- **File:** lot/src/macos/seatbelt.rs — line 193
- Comment claims "SBPL uses last-match-wins." Apple's SBPL uses most-specific-match-wins semantics. Code behavior is correct; only the comment is wrong.

---

## Group 9: Test coverage & conventions

### 9.1 Tests use `TempDir::new()` instead of project-local `test_tmp/`
- **Files:** lot/src/policy_builder.rs (354-356), lot/src/policy.rs (417-419), lot/src/env_check.rs (201-229)
- Inconsistent with project convention requiring `TempDir::new_in()` with `test_tmp/`.

### 9.2 `wait_with_output_timeout` has no dedicated test
- **File:** lot/src/lib.rs — lines 440-480
- Timeout path, `spawn_blocking` interaction, and panic propagation are untested.

### 9.3 `build_policy` tests missing coverage
- **File:** lot-cli/src/config.rs — lines 65-103
- No coverage for `exec_path`, `deny_path`, `include_platform_exec`, `include_platform_lib`, `include_temp`, `sentinel_dir`.

### 9.4 Memory limit test silently skips on macOS
- **File:** lot/tests/integration.rs — lines 1278-1329
- Silently returns on macOS setrlimit failure. Could give false confidence.

### 9.5 Windows symlink test silently skips without Developer Mode
- **File:** lot/tests/integration.rs — lines 1370-1399
- Silently returns if symlink creation fails. Never executes assertions on CI without Developer Mode.
