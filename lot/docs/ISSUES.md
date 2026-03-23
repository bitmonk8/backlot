# Issues

*Imported from audit findings — 2026-03-23. Verified and triaged 2026-03-23.*

## Group 6 — Linux cgroup cleanup leak (fallback path only)

### 6.1 NON-CRIT — [Correctness] Cgroup drain loop can leak cgroup directory on fallback path
- **File:** lot/src/linux/cgroup.rs:256-280
- **Description:** On the fallback path (kernel < 5.14, no `cgroup.kill`), `kill_all()` sends per-PID SIGKILL once. The drain loop does not retry `kill_all()`. A process that enters the cgroup after the single `kill_all()` call causes the drain loop to spin for 1 second, then `remove_dir` fails and the cgroup directory is leaked. On the primary path (`cgroup.kill`), this is a non-issue — the kernel atomically kills and freezes the cgroup.
