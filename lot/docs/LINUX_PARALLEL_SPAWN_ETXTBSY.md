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

## Linux spawn path

`lot::spawn()` on Linux (`lot/src/linux/mod.rs`) does the following:

1. `fork()` a helper process.
2. Helper calls `unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | ...)`.
3. Helper calls `setup_mount_namespace()` (`lot/src/linux/namespace.rs`),
   which creates a tmpfs root at `/tmp/lot-newroot-{pid}` and bind-mounts:
   - System directories (`/lib`, `/usr/lib`, `/bin`, `/usr/bin`, `/etc`)
   - Policy `read_paths` (read-only bind mount)
   - Policy `write_paths` (read-write bind mount)
   - Policy `exec_paths` (read-only + exec bind mount)
4. Helper `fork()`s again to create the inner child (PID 1 in new
   namespace).
5. Inner child calls `finish_mount_namespace()` (mounts `/proc`,
   `pivot_root`).
6. Inner child calls `execve()` on the binary.

The binary is **not copied** — it is bind-mounted from the host filesystem
into the namespace. Each helper gets a unique root path via `getpid()`.

## ETXTBSY semantics

On Linux, `execve` returns `ETXTBSY` when:

> The executable is open for writing by one or more processes.
> — execve(2)

This means some process has a write file descriptor open on the binary
being exec'd. The kernel tracks this at the inode level, not the path
level.

## Hypothesis

The exact mechanism is not confirmed, but likely candidates:

1. **`std::fs::copy` race at inode level:** `std::fs::copy` on Linux uses
   `copy_file_range` or `sendfile`, which opens the destination for write.
   Even though each test copies to a distinct path, if the filesystem
   deduplicates at the block/inode level (e.g., on a CoW filesystem like
   btrfs, or within a container overlay), the kernel might see multiple
   paths as the same inode, causing a write-open on one copy to block
   `execve` on another.

2. **Bind-mount propagation:** The `exec_paths` bind-mount connects the
   per-test cache dir into the namespace. If bind-mount setup in one
   namespace interacts with the source directory while another process is
   writing to a different file in a sibling directory under the same parent,
   the kernel's mount propagation could briefly mark the source as busy.

3. **Timing of copy vs spawn across threads:** Although `tmp_sandbox_cache()`
   completes before `NuSession::spawn()` is called within a single test,
   Rust's parallel test runner executes multiple tests concurrently. Test A's
   `std::fs::copy` to `cache_A/nu` might race with test B's `execve` of
   `cache_B/nu` if the underlying filesystem resolves both to related
   structures.

## Impact

- **Linux CI must use `--test-threads=1`** for tests that spawn sandboxed
  processes.
- **Windows and macOS are unaffected** — AppContainer and Seatbelt do not
  use mount namespaces or bind-mounts, so no inode-level conflicts occur.

## Workaround

Run tests sequentially on Linux:

```
cargo test -- --test-threads=1
```

## Potential fixes (not implemented)

1. **Retry with backoff:** Catch `ETXTBSY` in the spawn error path and
   retry after a short delay. Simple but masks the root cause.

2. **Pre-copy all binaries before any test spawns:** Use a `once_cell` or
   test fixture to copy all binaries before the parallel test phase begins,
   ensuring no `std::fs::copy` overlaps with any `execve`.

3. **Hardlink instead of copy:** `std::fs::hard_link` does not open the
   file for writing. Each test would get a distinct path backed by the same
   inode (intentionally). Since `execve` only checks for write-open fds,
   not link count, this might avoid the race. However, hard links would
   mean `exec_paths` bind-mounts in different namespaces point to the same
   inode — which could reintroduce the problem if bind-mount setup itself
   triggers inode-level locking.

4. **Investigate the kernel code path:** Determine exactly which operation
   sets the `ETXTBSY` flag. `fs/exec.c:deny_write_access()` checks
   `inode->i_writecount`. Identifying which operation increments
   `i_writecount` during parallel spawn would pinpoint the root cause.
