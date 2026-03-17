# Linux: ETXTBSY when spawning the same binary in parallel

## Summary

Concurrent `lot::spawn()` calls on Linux that execute the same binary (or
copies of the same binary) can fail with `ETXTBSY` (errno 26, "Text file
busy") at the `execve` step.

## Observed behavior

When running reel's test suite with parallel test threads on Linux CI
(GitHub Actions `ubuntu-latest`), ~7 of 25 nu-process-spawning tests fail:

```
sandbox setup failed: child setup failed at step 'execve': Text file busy (os error 26)
```

The same tests pass when run sequentially (`--test-threads=1`) or in
parallel on Windows (AppContainer) and macOS (Seatbelt).

## Test isolation design

Each test is designed for parallel execution:

- **Per-test cache directory:** `tmp_sandbox_cache()` copies the nu binary
  from the build-time `NU_CACHE_DIR` into a fresh `TempDir` per test. Each
  test's `lot::spawn()` executes a physically distinct file.
- **Per-test project directory:** `tmp_sandbox_project()` creates a unique
  `TempDir` for each test's working directory.
- **Per-test NuSession:** `isolated_session()` creates a new `NuSession`
  with its own cache dir for each test.
- No shared global or static state.

Despite this isolation, `ETXTBSY` occurs.

## ETXTBSY semantics

On Linux, `execve` returns `ETXTBSY` when:

> The executable is open for writing by one or more processes.
> — execve(2)

The kernel checks `inode->i_writecount` in `fs/exec.c:deny_write_access()`.
Any process holding a write-open fd on the inode — regardless of which path
it used to open it — blocks `execve` on that inode.

## Root cause: inherited write fds across fork()

The problem is **fd table inheritance across `fork()` in a multi-threaded
process**.

Rust's parallel test runner runs all tests in a single process with multiple
threads. `lot::spawn()` calls `fork()` to create the
helper process. `fork()` duplicates the **entire fd table** of the calling
process, including fds held by other threads.

### The race

1. **Thread A** calls `std::fs::copy()` to copy the nu binary to
   `/tmp/test_A/nu`. This opens the destination for writing. (The fd has
   `O_CLOEXEC` via Rust's stdlib, but that is irrelevant — `O_CLOEXEC`
   takes effect at `execve`, not `fork`.)

2. **Thread B** calls `lot::spawn()` → `fork()`. The helper process
   inherits **all** fds from the parent, including Thread A's write-open fd
   to `/tmp/test_A/nu`.

3. The helper closes only the fds it knows about: parent pipe ends and
   parent stdio fds. It **does not close** the inherited write fd.

4. The helper calls `fork()` again for the inner child, then blocks on
   `waitpid()`. It holds the write fd for the **entire lifetime** of the
   sandboxed process.

5. Thread A finishes `std::fs::copy()`, closes its fd in the parent
   process. Thread A then calls `lot::spawn()` → inner child →
   `execve("/tmp/test_A/nu")`.

6. Kernel checks `inode->i_writecount` on `/tmp/test_A/nu`. Thread B's
   helper still has a write fd open on that inode → **ETXTBSY**.

### Why the inner child's O_CLOEXEC doesn't help

The inner child (created by the helper's second `fork()`) also inherits the
stray write fd. When the inner child calls `execve`, `O_CLOEXEC` closes it
**in the inner child's process**. But the helper process never calls
`execve` — it just does `waitpid()`. The helper retains the write fd for
its entire lifetime.

### Why per-test isolation doesn't help

Each test copies to a distinct path and gets a distinct inode. The leak
is not path-based — it is fd-based. `fork()` copies the fd table at a
point-in-time snapshot. If any thread holds a write fd at that instant,
the forked child inherits it.

### Why it only affects Linux

macOS and Windows backends do not use `fork()` for process creation.
macOS uses `fork()`+`exec()` where the forked child applies seatbelt and
immediately execs (closing `O_CLOEXEC` fds). Windows uses
`CreateProcessW` which does not inherit arbitrary fds.

On Linux, the helper process sits in `waitpid()` without ever calling
`execve`, so inherited `O_CLOEXEC` fds are never closed.

## Impact (before fix)

- **Linux CI must use `--test-threads=1`** for tests that spawn sandboxed
  processes.
- **Windows and macOS are unaffected.**

## Fix: `close_range` in the helper (implemented)

The helper process now calls `close_range` (Linux 5.9+) immediately after
`fork()` to close all file descriptors >= 3 except the ones it needs:

- `err_pipe_wr`
- `child_stdin`, `child_stdout`, `child_stderr` (if piped)

The implementation in `linux/mod.rs:close_inherited_fds()` sorts the kept
fds and calls `close_range` in segments around them, avoiding heap
allocation (allocator state is unreliable post-fork).

If `close_range` is unavailable (kernel < 5.9), the call is a no-op and
the ETXTBSY race remains possible. `close_range` is available on all
kernels since 5.9 (2020), which covers all supported CI and production
targets.

### Alternatives considered

- **Iterate `/proc/self/fd`:** More portable but requires readdir
  post-fork.
- **Pre-copy binaries before tests:** Test-side fix only; doesn't protect
  arbitrary callers of `lot::spawn()`.
