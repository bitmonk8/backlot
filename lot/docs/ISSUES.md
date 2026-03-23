# Issues

*Imported from audit findings — 2026-03-23. Verified and triaged 2026-03-23.*

## Group 5 — Windows ACL error handling

### 5.1 NON-CRIT — [Error-Handling] `GetAce` failure silently swallowed
- **File:** lot/src/windows/acl_helpers.rs:371-372
- **Description:** `GetAce` failure is silently continued. `dacl_has_ace_for_sid` returns `Ok(false)`, causing `grant_traverse` to add a duplicate ACE instead of surfacing the error. In practice, `GetAce` with a valid DACL and in-range index should not fail — failure implies memory corruption, making any behavior unreliable regardless.

---

## Group 6 — Linux cgroup cleanup leak (fallback path only)

### 6.1 NON-CRIT — [Correctness] Cgroup drain loop can leak cgroup directory on fallback path
- **File:** lot/src/linux/cgroup.rs:256-280
- **Description:** On the fallback path (kernel < 5.14, no `cgroup.kill`), `kill_all()` sends per-PID SIGKILL once. The drain loop does not retry `kill_all()`. A process that enters the cgroup after the single `kill_all()` call causes the drain loop to spin for 1 second, then `remove_dir` fails and the cgroup directory is leaked. On the primary path (`cgroup.kill`), this is a non-issue — the kernel atomically kills and freezes the cgroup.
