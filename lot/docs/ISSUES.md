# Issues

*4 findings remaining from full project audit (2026-03-22). Low-severity findings removed 2026-03-23.*

---

## Group 1: Windows ACL/DACL Defensiveness

### 1.1 [Medium/Correctness] HANDLE leak in `traverse_acl.rs` — `lot/src/windows/traverse_acl.rs:417-423`
HANDLE from `CreateFileW` not in RAII guard. Leaks on panic or early return between open and `CloseHandle`.

### 1.2 [Medium/Error-handling] NTSTATUS formatted incorrectly — `lot/src/windows/traverse_acl.rs:425-437`
Negative NTSTATUS formatted as i32 hex produces wrong output (e.g., `0xC0000022` renders as `-0x3FFFFDDE`). Should cast to u32 before formatting.

---

## Group 2: Test Coverage — Windows

### 2.1 [Medium/Testing] Trailing backslash in program path untested — `lot/src/windows/cmdline.rs:243-249`
Produces malformed command line; no test coverage.

---

## Group 3: Test Coverage — Cross-Platform & Misc

### 3.1 [Medium/Testing] `wait_with_output_timeout` untested — `lot/src/lib.rs:440-480`
Contains non-trivial logic: `spawn_blocking`, `tokio::select!`, panic-resume, `kill_by_pid` on timeout.

---
