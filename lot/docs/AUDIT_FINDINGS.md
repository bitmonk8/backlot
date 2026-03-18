# Audit Findings

## Summary

| Severity | Count |
|----------|-------|
| Critical | 0     |
| High     | 0     |
| Medium   | 0     |
| Low      | 10    |
| **Total** | **10** |

| Category | Count |
|----------|-------|
| Correctness | 1 |
| Security | 1 |
| Error Handling | 2 |
| Resource Management | 2 |
| API Design | 2 |
| Code Organization | 2 |

---

## Correctness

### [Correctness] File: lot/src/windows/appcontainer.rs
- **Line(s):** 79-87
- **Description:** `unique_profile_name()` uses `AtomicU64` static counter. This violates the project's "no globals/statics/singletons" dependency rule from CLAUDE.md. Functionally, the PID+tick+counter combination is robust against collisions, so this is a code style violation rather than a bug.
- **Severity:** Low

---

## Security

### [Security] File: lot/src/macos/mod.rs
- **Line(s):** 28-63
- **Description:** macOS resource limits use `setrlimit` (RLIMIT_AS, RLIMIT_NPROC, RLIMIT_CPU) which the sandboxed process can lower further but the child process could potentially work around RLIMIT_AS by using `mmap` with `MAP_NORESERVE`. These limits are softer than Linux cgroups or Windows Job Objects.
- **Severity:** Low

---

## Error Handling

### [Error Handling] File: lot/src/linux/mod.rs
- **Line(s):** 234-237
- **Description:** The `helper_bail` macro accesses `*libc::__errno_location()` directly. If `unshare()` sets errno but a subsequent signal handler or other operation changes it before the macro reads it, the wrong errno will be reported. In practice this is unlikely in the single-threaded helper, but the pattern is fragile.
- **Severity:** Low

### [Error Handling] File: lot/src/policy.rs
- **Line(s):** 192-244
- **Description:** `validate()` reports only the first error found. If a policy has multiple problems (e.g., nonexistent path and zero resource limit), the caller must fix and re-validate iteratively. Collecting all errors would be more user-friendly.
- **Severity:** Low

---

## Resource Management

### [Resource Management] File: lot/src/linux/cgroup.rs
- **Line(s):** 108-118
- **Description:** `CgroupGuard::new()` uses `clock_gettime(CLOCK_MONOTONIC)` nanoseconds + PID for uniqueness. Two calls in the same nanosecond from the same PID (theoretically possible on fast hardware) would collide. The `fs::create_dir` call would fail with EEXIST, propagating as an IO error. This is extremely unlikely but possible under high concurrency.
- **Severity:** Low

### [Resource Management] File: lot/src/windows/appcontainer.rs
- **Description:** Child-side pipe handle closing logic is correct but not obvious. Inherited handles are not closed, Null handles are INVALID_HANDLE_VALUE and handled by `close_handle_if_valid`.
- **Severity:** Low

---

## API Design

### [API Design] File: lot/src/lib.rs
- **Description:** Windows-specific functions (`grant_appcontainer_prerequisites_for_policy`, `appcontainer_prerequisites_met_for_policy`, `is_elevated`, etc.) are re-exported at the crate root, polluting the API surface for non-Windows users. These should be behind a `windows` module or feature-gated more cleanly.
- **Severity:** Low

### [API Design] File: lot/src/command.rs
- **Line(s):** 37-47
- **Description:** `SandboxCommand::new()` defaults stdin to `Null` but stdout/stderr to `Piped`. This asymmetry is documented but may surprise callers who expect either all-null or all-piped defaults. Consider making all three default to `Null` (safest) or all to `Piped` (most useful).
- **Severity:** Low

---

## Code Organization

### [Code Organization] File: lot/src/windows/nul_device.rs
- **Description:** This module combines NUL device ACE management, `is_elevated()`, `sddl_has_ac_allow()` parsing, and the public `grant_appcontainer_prerequisites` / `appcontainer_prerequisites_met` API. The `is_elevated()` function and the public prerequisite API are not specific to the NUL device and belong at a higher level.
- **Severity:** Low

### [Code Organization] File: lot/src/windows/mod.rs
- **Line(s):** 7-9
- **Description:** `FILE_GENERIC_READ`, `FILE_GENERIC_WRITE`, `FILE_GENERIC_EXECUTE` are defined as raw constants rather than using `windows-sys` feature flags. The comment explains this is to avoid extra feature dependencies, which is a valid trade-off, but these constants should have comments linking to the MSDN documentation for verification.
- **Severity:** Low
