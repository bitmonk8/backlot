# Audit Findings

## Summary

| Severity | Count |
|----------|-------|
| Critical | 3     |
| High     | 8     |
| Medium   | 14    |
| Low      | 10    |
| **Total** | **35** |

| Category | Count |
|----------|-------|
| Correctness | 8 |
| Security | 6 |
| Error Handling | 4 |
| Resource Management | 5 |
| API Design | 4 |
| Testing | 3 |
| Code Organization | 3 |
| Documentation | 2 |

---

## Correctness

### [Correctness] File: lot/src/linux/cgroup.rs
- **Line(s):** 156-180
- **Description:** `kill_all()` fallback path has a PID recycling race. Between reading a PID from `cgroup.procs` and sending SIGKILL, the process may have exited and the PID reassigned to an unrelated process. The code documents this but does not mitigate it (e.g., by using `pidfd_open` on kernels that support it).
- **Severity:** Critical

### [Correctness] File: lot/src/linux/mod.rs
- **Line(s):** 143-147
- **Description:** When `cgroup::available()` returns true but `CgroupGuard::new()` fails, the error is silently swallowed via `.ok()`. Resource limits specified by the caller are silently not enforced. The caller has no indication that their `max_memory_bytes`, `max_processes`, or `max_cpu_seconds` were ignored.
- **Severity:** Critical

### [Correctness] File: lot/src/linux/mod.rs
- **Line(s):** 265-281
- **Description:** If moving the helper into the cgroup fails (fd open or write fails), the code silently continues without resource limits. Combined with the silent `.ok()` above, there is no path that reports cgroup failures to the caller.
- **Severity:** High

### [Correctness] File: lot/src/linux/namespace.rs
- **Line(s):** 123-129
- **Description:** Policy `read_paths` that contain non-UTF-8 components are silently skipped by the `path.to_str()` check. The sandbox would run without mounting those paths, silently violating the caller's policy. The same applies to `write_paths` and `exec_paths` (lines 132-147).
- **Severity:** High

### [Correctness] File: lot/src/windows/appcontainer.rs
- **Line(s):** 79-87
- **Description:** `unique_profile_name()` uses `AtomicU64` static counter. This violates the project's "no globals/statics/singletons" dependency rule from CLAUDE.md. Functionally, the PID+tick+counter combination is robust against collisions, so this is a code style violation rather than a bug.
- **Severity:** Low

### [Correctness] File: lot/src/macos/seatbelt.rs
- **Line(s):** 269
- **Description:** `escape_sbpl_path` uses `path.display().to_string()` which may produce lossy output for non-UTF-8 paths. On macOS this is unlikely to be a problem (HFS+/APFS use UTF-8), but the function returns `Ok` for paths that were silently corrupted during conversion.
- **Severity:** Medium

### [Correctness] File: lot/src/windows/appcontainer.rs
- **Line(s):** 236-248
- **Description:** `sd_to_sddl` assumes `sddl_len` includes the null terminator and subtracts 1. The Windows documentation is ambiguous on whether `sddl_len` includes the null. If it does not, this produces an off-by-one that truncates the last character. The `nul_device.rs` version (lines 140-148) measures manually with a pointer walk, avoiding this ambiguity. The two implementations should be consistent.
- **Severity:** Medium

### [Correctness] File: lot-cli/src/main.rs
- **Line(s):** 223-227
- **Description:** `exit_code_from_status` truncates exit codes above 255 to `1` via `u8::try_from`. On Windows, exit codes can be 32-bit values (e.g., `0xC0000005` for access violation). The CLI will return 1 for these instead of a more informative value.
- **Severity:** Medium

---

## Security

### [Security] File: lot/src/linux/seccomp.rs
- **Line(s):** 178-179
- **Description:** `prctl` is allowed unconditionally. While seccomp filters are additive (new filters can only be more restrictive) and `PR_SET_NO_NEW_PRIVS` is already set, allowing all prctl operations is overly broad. `PR_SET_DUMPABLE`, `PR_SET_NAME`, and other operations could be useful for evasion or information gathering. Consider allowlisting specific prctl operations via argument filtering.
- **Severity:** Medium

### [Security] File: lot/src/linux/seccomp.rs
- **Line(s):** 180
- **Description:** `ioctl` is allowed unconditionally. ioctl can perform device-specific operations that could escape containment if device files are accessible. The mount namespace limits exposure, but this is still a broader allowance than necessary for most sandboxed workloads.
- **Severity:** Medium

### [Security] File: lot/src/macos/seatbelt.rs
- **Line(s):** 157
- **Description:** `mach-lookup` is allowed unconditionally. This permits the sandboxed process to communicate with any Mach service. The code documents this is intentional (Chrome/Firefox do the same), but it is a known weakness of the macOS sandbox that enables IPC-based escapes.
- **Severity:** Medium

### [Security] File: lot/src/macos/mod.rs
- **Line(s):** 28-63
- **Description:** macOS resource limits use `setrlimit` (RLIMIT_AS, RLIMIT_NPROC, RLIMIT_CPU) which the sandboxed process can lower further but the child process could potentially work around RLIMIT_AS by using `mmap` with `MAP_NORESERVE`. These limits are softer than Linux cgroups or Windows Job Objects.
- **Severity:** Low

### [Security] File: lot/src/linux/namespace.rs
- **Line(s):** 98-113
- **Description:** System library and bin directories (`/lib`, `/usr/lib`, `/bin`, `/usr/bin`, etc.) are always bind-mounted into the sandbox regardless of policy. This provides a large attack surface of available binaries and libraries. A more restrictive approach would only mount directories explicitly referenced in the policy.
- **Severity:** High

### [Security] File: lot/src/linux/namespace.rs
- **Line(s):** 116-120
- **Description:** `/etc` is always bind-mounted read-only into the sandbox. This exposes `/etc/passwd`, `/etc/shadow` (if readable), `/etc/hostname`, and other sensitive system configuration to the sandboxed process. Only specific files like `/etc/ld.so.cache` and `/etc/nsswitch.conf` are needed for the dynamic linker.
- **Severity:** High

---

## Error Handling

### [Error Handling] File: lot/src/linux/mod.rs
- **Line(s):** 234-237
- **Description:** The `helper_bail` macro accesses `*libc::__errno_location()` directly. If `unshare()` sets errno but a subsequent signal handler or other operation changes it before the macro reads it, the wrong errno will be reported. In practice this is unlikely in the single-threaded helper, but the pattern is fragile.
- **Severity:** Low

### [Error Handling] File: lot/src/windows/appcontainer.rs
- **Line(s):** 604-626
- **Description:** `restore_from_sentinel` collects errors from ACL restoration into a single string joined with "; ". If restoration fails for multiple paths, the caller gets one combined error with no structured way to identify which paths failed. This makes debugging difficult.
- **Severity:** Medium

### [Error Handling] File: lot/src/macos/mod.rs
- **Line(s):** 114-122
- **Description:** The macOS `child_bail` macro writes only 4 bytes (errno) vs. the Linux `helper_bail` which writes 8 bytes (step+errno). The macOS error path therefore provides no information about which step failed, making debugging harder.
- **Severity:** Medium

### [Error Handling] File: lot/src/policy.rs
- **Line(s):** 192-244
- **Description:** `validate()` reports only the first error found. If a policy has multiple problems (e.g., nonexistent path and zero resource limit), the caller must fix and re-validate iteratively. Collecting all errors would be more user-friendly.
- **Severity:** Low

---

## Resource Management

### [Resource Management] File: lot/src/unix.rs
- **Line(s):** 150-188
- **Description:** `setup_stdio_pipes` does not clean up previously created pipes if a later pipe creation fails. For example, if stdout pipe creation succeeds but stderr pipe creation fails, the stdout pipe fds are leaked. Each pipe creation should have a cleanup path for the previously created fds.
- **Severity:** High

### [Resource Management] File: lot/src/linux/cgroup.rs
- **Line(s):** 108-118
- **Description:** `CgroupGuard::new()` uses `clock_gettime(CLOCK_MONOTONIC)` nanoseconds + PID for uniqueness. Two calls in the same nanosecond from the same PID (theoretically possible on fast hardware) would collide. The `fs::create_dir` call would fail with EEXIST, propagating as an IO error. This is extremely unlikely but possible under high concurrency.
- **Severity:** Low

### [Resource Management] File: lot/src/windows/appcontainer.rs
- **Line(s):** 1428-1436
- **Description:** Child-side pipe handles are only closed when `command.stdin == SandboxStdio::Piped` (and similarly for stdout/stderr). If stdin is `Inherit`, `child_stdin` is the actual stdin handle from `GetStdHandle`. This handle should NOT be closed. The code is correct in its conditional closing, but the inherited handle case for `Null` (where `child_stdin == INVALID_HANDLE_VALUE`) is also correctly handled by `close_handle_if_valid`. This is fine but the logic is not obvious.
- **Severity:** Low

### [Resource Management] File: lot/src/linux/mod.rs
- **Line(s):** 465-474
- **Description:** `LinuxSandboxedChild` uses `Cell<bool>` for the `waited` flag. `Cell` is `!Sync`, which means `LinuxSandboxedChild` cannot be shared across threads. However, since the struct does not implement `Sync` explicitly and is only used through `SandboxedChild` which also doesn't impl `Sync`, this is acceptable. The `wait()` method takes `&self` rather than `&mut self`, so concurrent `wait()` calls could race on `waitpid`. Using `&mut self` for `wait()` would be safer.
- **Severity:** High

### [Resource Management] File: lot/src/macos/mod.rs
- **Line(s):** 228-234
- **Description:** Same issue as Linux: `MacSandboxedChild` uses `Cell<bool>` for `waited`, and `wait()` takes `&self`. Two concurrent callers of `wait()` could both call `waitpid`, where the second call would get ECHILD. The `&self` signature is required by the `platform_dispatch` macro but creates a correctness hazard.
- **Severity:** High

---

## API Design

### [API Design] File: lot/src/policy.rs
- **Line(s):** 13-28
- **Description:** `SandboxPolicy` has all public fields, allowing callers to construct invalid policies and bypass validation. Making fields private and requiring construction through `SandboxPolicyBuilder` (or a validated constructor) would enforce invariants at the type level.
- **Severity:** Medium

### [API Design] File: lot/src/lib.rs
- **Line(s):** 119-139
- **Description:** Windows-specific functions (`grant_appcontainer_prerequisites_for_policy`, `appcontainer_prerequisites_met_for_policy`, `is_elevated`, etc.) are re-exported at the crate root, polluting the API surface for non-Windows users. These should be behind a `windows` module or feature-gated more cleanly.
- **Severity:** Low

### [API Design] File: lot/src/lib.rs
- **Line(s):** 186-195
- **Description:** `SandboxedChild::kill()` takes `&self` but semantically performs a one-shot operation. Taking `&mut self` would prevent concurrent kill+wait races and better express the mutation. The current `&self` signature is driven by the `platform_dispatch` macro.
- **Severity:** Medium

### [API Design] File: lot/src/command.rs
- **Line(s):** 37-47
- **Description:** `SandboxCommand::new()` defaults stdin to `Null` but stdout/stderr to `Piped`. This asymmetry is documented but may surprise callers who expect either all-null or all-piped defaults. Consider making all three default to `Null` (safest) or all to `Piped` (most useful).
- **Severity:** Low

---

## Testing

### [Testing] File: lot/src/linux/seccomp.rs
- **Line(s):** 36 (entire file)
- **Description:** Seccomp filter is only built and tested on `x86_64`. The `#[cfg(target_arch = "x86_64")]` gate means `aarch64` Linux (e.g., AWS Graviton, Apple Silicon Linux VMs) gets no seccomp protection at all. There is no CI job for `aarch64-unknown-linux-gnu`.
- **Severity:** High

### [Testing] File: lot/tests/integration.rs
- **Line(s):** 1-641
- **Description:** Integration tests do not cover the `tokio` timeout feature (`wait_with_output_timeout`). The `#[cfg(feature = "tokio")]` tests are only in `lib.rs` unit tests. CI does not appear to run tests with `--features tokio`.
- **Severity:** Medium

### [Testing] File: lot/src/linux/cgroup.rs
- **Line(s):** 223-295
- **Description:** Cgroup tests require specific system configuration (cgroupv2 with delegation) and are gated with `assert!(available())`. If the CI runner doesn't have cgroups configured, these tests silently pass the assertion and skip the real logic, providing false confidence. The tests should use `#[ignore]` or a custom test attribute when cgroups are unavailable.
- **Severity:** Medium

---

## Code Organization

### [Code Organization] File: lot/src/windows/appcontainer.rs
- **Line(s):** 1-1600+
- **Description:** At 1600+ lines, `appcontainer.rs` is the largest file in the codebase. It combines AppContainer profile management, ACL manipulation, sentinel file I/O, pipe creation, command-line building, security capabilities, and process creation. Extracting sentinel management, pipe helpers, and command-line building into separate modules would improve navigability.
- **Severity:** Medium

### [Code Organization] File: lot/src/windows/nul_device.rs
- **Line(s):** 1-439
- **Description:** This module combines NUL device ACE management, `is_elevated()`, `sddl_has_ac_allow()` parsing, and the public `grant_appcontainer_prerequisites` / `appcontainer_prerequisites_met` API. The `is_elevated()` function and the public prerequisite API are not specific to the NUL device and belong at a higher level.
- **Severity:** Low

### [Code Organization] File: lot/src/windows/mod.rs
- **Line(s):** 7-9
- **Description:** `FILE_GENERIC_READ`, `FILE_GENERIC_WRITE`, `FILE_GENERIC_EXECUTE` are defined as raw constants rather than using `windows-sys` feature flags. The comment explains this is to avoid extra feature dependencies, which is a valid trade-off, but these constants should have comments linking to the MSDN documentation for verification.
- **Severity:** Low

---

## Documentation

### [Documentation] File: lot/src/linux/cgroup.rs
- **Line(s):** 133-137
- **Description:** `max_cpu_seconds` is silently ignored on Linux. The comment explains why (cgroupv2 `cpu.max` controls bandwidth, not total time), but this is not documented in the public `ResourceLimits` struct or the crate-level docs. Callers setting `max_cpu_seconds` on Linux get no warning that it has no effect.
- **Severity:** High

### [Documentation] File: lot/src/policy.rs
- **Line(s):** 248-257
- **Description:** `ResourceLimits` doc comments say "None = no limit" but do not mention that `max_cpu_seconds` is only enforced on Windows (via Job Objects) and macOS (via RLIMIT_CPU). Linux ignores it entirely. This is a doc-implementation mismatch that could lead to false security assumptions.
- **Severity:** High
