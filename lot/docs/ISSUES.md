# Issues

*2 findings remaining from full project audit (2026-03-22). Low-severity findings removed 2026-03-23.*

---

## Group 2: Test Coverage — Windows

### 2.1 [Medium/Testing] Trailing backslash in program path untested — `lot/src/windows/cmdline.rs:243-249`
Produces malformed command line; no test coverage.

---

## Group 3: Test Coverage — Cross-Platform & Misc

### 3.1 [Medium/Testing] `wait_with_output_timeout` untested — `lot/src/lib.rs:440-480`
Contains non-trivial logic: `spawn_blocking`, `tokio::select!`, panic-resume, `kill_by_pid` on timeout.

---
