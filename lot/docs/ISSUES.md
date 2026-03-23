# Issues

*Imported from audit findings — 2026-03-23. Verified and triaged 2026-03-23.*

## Group 2 — Linux namespace mount correctness

### 2.1 MUST FIX — [Correctness] `MS_REC` ignored on bind-mount remounts
- **File:** lot/src/linux/namespace.rs:388-389
- **Description:** `MS_REC` is ignored by the kernel on bind-mount remount operations. The remount only changes flags on the top-level mount point; submounts under a recursively-bound policy path retain their original flags. For `bind_mount_readonly`, submounts remain writable.

### 2.2 NON-CRIT — [Correctness] System symlinks not preserved in mount namespace
- **File:** lot/src/linux/namespace.rs:95-109
- **Description:** `/lib`, `/lib64`, `/bin`, `/sbin` are symlinks on modern distros (Fedora, Arch, recent Ubuntu). `bind_mount_exec` creates real directories instead of preserving symlinks, producing redundant bind mounts and altering observable path structure inside the sandbox. Functionally correct for dynamic linking but differs from host layout.

---

## Group 3 — Seccomp missing syscalls

### 3.1 MUST FIX — [Correctness] `close_range` not in seccomp allowlist
- **File:** lot/src/linux/seccomp.rs
- **Description:** `SYS_close_range` is missing. glibc 2.34+ and musl 1.2.4+ use `close_range` internally during `posix_spawn`, `popen`, and `closefrom`. Programs using these functions get `EPERM`. No additional attack surface beyond the already-allowed `SYS_close`.

### 3.2 MUST FIX — [Correctness] `prlimit64` not in seccomp allowlist
- **File:** lot/src/linux/seccomp.rs
- **Description:** `SYS_prlimit64` is missing. Modern glibc and musl implement `getrlimit`/`setrlimit` via the `prlimit64` syscall. Programs that query resource limits (including the Rust runtime for stack guard pages) get `EPERM`. Breaks most non-trivial sandboxed processes on contemporary distros.

---

## Group 4 — Unix `try_wait`/`kill` race condition

### 4.1 NON-CRIT — [Correctness] `try_wait` sets `waited` before `waitpid`, races with `kill`
- **File:** lot/src/unix.rs:756-776
- **Description:** `try_wait` uses `compare_exchange` to set `waited = true` before calling `waitpid(WNOHANG)`. If `waitpid` returns 0 (child still running), it resets to `false`. During this window, a concurrent `kill()` observes `waited == true` and returns `Ok(())` without sending SIGKILL. Fix: remove the `waited` early-return in `kill()` and let it unconditionally call `send_sigkill()` (ESRCH is already treated as success).

---

## Group 5 — Windows ACL error handling

### 5.1 NON-CRIT — [Error-Handling] `GetAce` failure silently swallowed
- **File:** lot/src/windows/acl_helpers.rs:371-372
- **Description:** `GetAce` failure is silently continued. `dacl_has_ace_for_sid` returns `Ok(false)`, causing `grant_traverse` to add a duplicate ACE instead of surfacing the error. In practice, `GetAce` with a valid DACL and in-range index should not fail — failure implies memory corruption, making any behavior unreliable regardless.

---

## Group 6 — Linux cgroup cleanup leak (fallback path only)

### 6.1 NON-CRIT — [Correctness] Cgroup drain loop can leak cgroup directory on fallback path
- **File:** lot/src/linux/cgroup.rs:256-280
- **Description:** On the fallback path (kernel < 5.14, no `cgroup.kill`), `kill_all()` sends per-PID SIGKILL once. The drain loop does not retry `kill_all()`. A process that enters the cgroup after the single `kill_all()` call causes the drain loop to spin for 1 second, then `remove_dir` fails and the cgroup directory is leaked. On the primary path (`cgroup.kill`), this is a non-issue — the kernel atomically kills and freezes the cgroup.

---

## Group 7 — Windows job object CPU limit overflow

### 7.1 NON-CRIT — [Correctness] `PerJobUserTimeLimit` wraps to negative on large `cpu_secs`
- **File:** lot/src/windows/job.rs:83-88
- **Description:** `cpu_secs.saturating_mul(10_000_000)` saturates to `u64::MAX` for `cpu_secs >= 1_844_674_408` (~58 years), then `as i64` wraps to `-1`. The existing code comment claims wrapping is acceptable, but a negative `PerJobUserTimeLimit` changes semantics from "very large limit" to immediate termination. Fix: clamp to `i64::MAX` before the cast.

---

## Group 8 — Documentation mismatches

### 8.1 NON-CRIT — [Doc-Mismatch] README license badge says MIT only, repo is dual-licensed
- **File:** README.md:5,199
- **Description:** README badge and footer claim "MIT" only. `Cargo.toml` declares `MIT OR Apache-2.0`. Both license files exist. Badge links to nonexistent `LICENSE` file.

### 8.2 NON-CRIT — [Doc-Mismatch] `grant_paths()` doc comment is stale
- **File:** lot/src/policy.rs:251
- **Description:** Doc comment says `grant_paths()` is "used for computing ancestors that need traverse ACEs." The traverse ACE code uses `all_paths()`. The only caller is `env_check.rs` for PATH reachability.

### 8.3 NON-CRIT — [Doc-Mismatch] PATH error message omits `write_path` as valid fix
- **File:** lot/src/env_check.rs:134-135
- **Description:** Error message tells users to add entries as `read_path/exec_path`, but `grant_paths()` includes `write_paths` too. A directory in `write_paths` would pass the check, so a user would never hit this error for write paths. The omission is technically incomplete but not practically misleading — `read_path` or `exec_path` are the natural choices for PATH directories.

### 8.4 NON-CRIT — [Doc-Mismatch] DESIGN.md claims `InternetClientServer` capability exists
- **File:** docs/DESIGN.md:164
- **Description:** DESIGN.md states network access can use `InternetClient` or `InternetClientServer`. The implementation only creates `InternetClient`. `InternetClientServer` is not implemented.
