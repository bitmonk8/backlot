# Design & Architecture

## Project Structure

```
lot/                           (workspace root)
├── Cargo.toml                 (workspace config, shared lints/versions/profile)
├── lot/                       (library crate)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs             — Public API: spawn(), probe(), types
│   │   ├── policy.rs          — SandboxPolicy, ResourceLimits, validation
│   │   ├── policy_builder.rs  — SandboxPolicyBuilder (auto-canonicalization, platform defaults)
│   │   ├── command.rs         — SandboxCommand builder
│   │   ├── error.rs           — SandboxError
│   │   ├── unix.rs            — Shared Unix helpers (pipes, stdio, UnixSandboxedChild lifecycle, child_bail)
│   │   ├── linux/
│   │   │   ├── mod.rs         — LinuxSandbox: orchestrates namespace + seccomp + cgroup
│   │   │   ├── namespace.rs   — clone(), pivot_root, bind mounts, uid/gid mapping
│   │   │   ├── seccomp.rs     — BPF filter construction and application
│   │   │   └── cgroup.rs      — cgroup v2 creation, limit writes, cleanup
│   │   ├── macos/
│   │   │   ├── mod.rs         — MacSandbox: fork + seatbelt + rlimit
│   │   │   └── seatbelt.rs    — SBPL profile generation, sandbox_init FFI
│   │   └── windows/
│   │       ├── mod.rs         — WindowsSandbox: AppContainer + job object
│   │       ├── appcontainer.rs — Profile lifecycle, capability assembly, ACL management, process creation
│   │       ├── job.rs         — Job object creation, resource limits
│   │       ├── acl_helpers.rs  — Shared DACL manipulation (SID allocation, ACE application)
│   │       ├── nul_device.rs  — NUL device ACE, prerequisites API
│   │       ├── traverse_acl.rs — Ancestor traverse ACE management
│   │       ├── sddl.rs        — SDDL/DACL helpers (get_sddl, restore_sddl)
│   │       ├── sentinel.rs    — Sentinel file ACL recovery (write, read, restore, find_stale_sentinels)
│   │       ├── elevation.rs   — is_elevated() check (UAC token inspection)
│   │       ├── pipe.rs        — Pipe creation and stdio handle helpers
│   │       └── cmdline.rs     — Command-line building and argument quoting
│   └── tests/
│       └── integration.rs
├── lot-cli/                   (CLI binary crate)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs            — lot run, lot setup, lot probe
├── docs/
├── prompts/
└── .github/
```

The workspace pattern (library + CLI) follows the same structure as sibling projects (reel, flick). The CLI is a thin wrapper; all sandbox logic lives in the library crate.

## Dependencies

| Crate | Platform | Purpose |
|---|---|---|
| `libc` | Linux, macOS | Syscall wrappers (`clone`, `setrlimit`, `prctl`, etc.) |
| `seccompiler` | Linux | seccomp-BPF filter construction |
| `windows-sys` | Windows | Win32 API bindings (AppContainer, Job Objects, ACL) |
| `thiserror` | All | Error derive |
| `tokio` | All | Async runtime for `wait_with_output_timeout` (optional `tokio` feature) |
| `clap` | CLI | Argument parsing |
| `serde` + `serde_yml` | CLI | YAML config deserialization |

---

## Platform Mechanisms

### Linux: Namespaces + seccomp-BPF + cgroups v2

**Namespaces** provide the primary isolation boundary:

| Namespace | Purpose |
|---|---|
| `CLONE_NEWUSER` | Unprivileged namespace creation. Maps caller UID/GID into child. |
| `CLONE_NEWNS` | Mount namespace. Private root with only allowed paths bind-mounted. |
| `CLONE_NEWPID` | PID namespace. Child sees itself as PID 1. Cannot signal host processes. |
| `CLONE_NEWNET` | Network namespace. Empty network stack by default (no interfaces). |
| `CLONE_NEWIPC` | IPC namespace. Isolates SysV IPC and POSIX message queues. |

**seccomp-BPF** filters syscalls:
- Allowlist of permitted syscalls.
- Default action: `EPERM` (not `SIGKILL` — debuggable).
- Conditional rules: network syscalls gated on policy.
- `prctl` filtered by arg0: only `PR_SET_NAME`, `PR_GET_NAME`, `PR_SET_PDEATHSIG`, `PR_GET_PDEATHSIG`, `PR_SET_TIMERSLACK`, `PR_GET_TIMERSLACK` allowed.
- `ioctl` filtered by arg1: only `TCGETS`, `TIOCGWINSZ`, `TIOCGPGRP`, `FIONREAD`, `FIOCLEX`, `FIONCLEX` allowed.

**cgroups v2** enforces resource limits using the sibling cgroup model (to respect the cgroupv2 "no internal processes" constraint):
- Memory limit (`memory.max`).
- CPU bandwidth (`cpu.max` — period/quota).
- PID limit (`pids.max` — prevent fork bombs).
- Cgroup created per sandbox invocation, cleaned up on drop.

**Filesystem setup:**
1. Create tmpfs at a temporary mount point.
2. Bind-mount allowed read-only paths with `MS_RDONLY | MS_NOSUID | MS_NODEV`.
3. Bind-mount allowed read-write paths with `MS_NOSUID | MS_NODEV`.
4. Bind-mount allowed executable paths with `MS_RDONLY | MS_NOSUID | MS_NODEV` (no `MS_NOEXEC`).
5. Overmount each deny path with an empty read-only tmpfs (`size=0`, `MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC`). Must happen after step 2–4 so the parent grant mount exists. The denied subtree appears as an empty directory; reads/writes/creates fail with ENOENT/EROFS.
6. `pivot_root` into the new root. Unmount old root.
7. Essential paths (`/proc`, `/dev/null`, `/dev/urandom`, dynamic linker paths) are always mounted read-only.

**Single-threaded constraint:** `CLONE_NEWUSER` requires the calling process to be single-threaded. Lot handles this by forking a single-threaded helper process before calling `clone()` with namespace flags. The caller (which may be multi-threaded/tokio) never directly enters the namespace. Mount namespace setup is split into two phases (helper + inner child) to correctly mount `/proc` after the PID namespace is active.

### macOS: Seatbelt (sandbox_init)

**Seatbelt** is macOS's kernel-level sandbox, configured via SBPL (Sandbox Profile Language) profiles.

FFI surface:
```c
int sandbox_init(const char *profile, uint64_t flags, char **errorbuf);
void sandbox_free_error(char *errorbuf);
```

**Profile generation:**
- Start with `(version 1) (deny default)`.
- Add `(allow file-read* (subpath "..."))` for read-only paths.
- Add `(allow file-read* (subpath "...")) (allow file-write* (subpath "..."))` for read-write paths.
- Add `(allow process-exec (subpath "..."))` for executable paths.
- Add `(allow network*)` if network permitted.
- Add `(deny file-read* ...)`, `(deny file-read-metadata ...)`, `(deny file-write* ...)`, `(deny process-exec ...)`, `(deny file-map-executable ...)` for deny paths. Must appear after allow rules (SBPL uses last-match-wins).
- Add `(allow file-read-metadata (literal "..."))` for each ancestor directory of all policy paths and the program binary. Enables `stat()`-based path traversal (e.g., nu_glob walking path components). Uses `literal` (exact match), not `subpath`. Excludes `/` and system paths already granted.
- Always allow: system libraries (`/usr/lib`, `/System/Library`), dynamic linker cache, `/dev/urandom`.
- `mach-lookup` is unrestricted (narrowing breaks most programs).
- `generate_profile` returns `Result<String, SandboxError>`. Path-encoding failures (non-UTF-8, null bytes) are propagated as errors rather than silently dropping rules, which would weaken the sandbox.

**Process model:** Fork a helper, call `setsid()` so the child becomes its own process group leader (enabling `killpg` to kill all descendants on drop/timeout), apply `sandbox_init` in the helper (permanent, no undo), then exec the target. The parent is never sandboxed.

**Resource limits:** `setrlimit(RLIMIT_AS, ...)` for memory. No cgroup equivalent on macOS.

**Deprecation note:** Apple deprecated `sandbox_init` but has not removed it. It is still used by major applications (Chrome, Firefox) and the underlying kernel sandbox is actively maintained. No replacement API exists for third-party use.

### Shared Unix lifecycle (`unix.rs`)

`UnixSandboxedChild` in `unix.rs` holds the common state (pid, stdio fds, `AtomicBool` waited flag) and implements `wait`, `try_wait`, `wait_with_output`, `kill`, `take_stdin/stdout/stderr`, `close_fds`, and `kill_and_reap`. The kill mechanism differs between Linux (`libc::kill` on helper PID; inner child dies via `PR_SET_PDEATHSIG`) and macOS (`libc::killpg` on child PGID after `setsid`). This is parameterized via `KillStyle` enum. Platform wrappers (`LinuxSandboxedChild`, `MacSandboxedChild`) delegate lifecycle methods and add platform-specific cleanup (cgroup guard on Linux).

The `child_bail` function (async-signal-safe, no allocations) writes an 8-byte `[step:i32, errno:i32]` error report to the error pipe and calls `_exit(1)`. Used by both platforms' forked child processes via a thin macro wrapper.

### Windows: AppContainer + Job Objects

**AppContainer** is a Windows kernel security boundary that denies access to all resources by default. Access is granted explicitly via:
- ACL entries on files/directories/registry keys, granting the AppContainer's package SID read or read-write access.
- Capability SIDs (e.g., `InternetClient` for network access).

**Process model:**
1. Create or open an AppContainer profile (`CreateAppContainerProfile`). Derives a unique package SID.
2. Build a `SECURITY_CAPABILITIES` structure with the package SID and desired capability SIDs.
3. Create stdio pipes (child and parent handles for stdin/stdout/stderr).
4. Call `CreateProcessW` with `STARTUPINFOEX` containing the security capabilities via `UpdateProcThreadAttribute`.
5. The child process runs inside the AppContainer boundary.

**Filesystem access:**
- Before launch, grant the package SID read or read-write ACL entries on allowed paths (`SetEntriesInAcl`, `SetNamedSecurityInfo`).
- For deny paths, add explicit deny ACEs (`DENY_ACCESS` with `FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE`, `SUB_CONTAINERS_AND_OBJECTS_INHERIT`). Windows evaluates explicit denies before explicit allows, so these override inherited grants from parent directories.
- On cleanup, remove the ACL entries (restore original DACLs from saved SDDL strings).

**Network access:**
- Denied by default inside AppContainer.
- Granted by adding `InternetClient` or `InternetClientServer` capability SIDs.

**Job Objects** enforce resource limits:
- `JOB_OBJECT_LIMIT_PROCESS_MEMORY` — memory cap.
- `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` — limit child process count.
- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` — RAII cleanup: closing the job handle kills all processes in the job.
- UI restrictions: block clipboard, desktop, display settings access.

**Sentinel file ACL recovery:**
1. Before granting ACLs, write a manifest of modified paths + original DACLs to a sentinel file.
2. On normal exit, restore ACLs and delete sentinel.
3. On next `spawn()`, check for stale sentinels from crashed sessions and restore ACLs before proceeding.
4. `cleanup_stale()` is exposed as a public function for explicit control.

---

## Windows: Ancestor Traverse ACEs

AppContainer sandboxed processes cannot call `fs::metadata()` on ancestor directories unless those directories have a traverse ACE for ALL APPLICATION PACKAGES. Programs that walk path components (e.g., glob-based path resolution, `create_dir_all`) fail without this.

### Spawn-time automatic grants

`spawn()` grants traverse ACEs on ancestor directories of all policy paths (both grant and deny paths) automatically. For each ancestor lacking the ACE, it attempts `grant_traverse()`. Failures on directories the user cannot modify (system directories, volume root) produce `SandboxError::PrerequisitesNotMet(String)` with a pre-formatted diagnostic message. The `_for_policy` prerequisite functions also cover deny paths to match `spawn_inner`'s behavior.

### ACE details

| Property | Value |
|----------|-------|
| Access mask | `FILE_TRAVERSE \| SYNCHRONIZE \| FILE_READ_ATTRIBUTES` |
| Inheritance | `NO_INHERITANCE` |
| Trustee | ALL APPLICATION PACKAGES (`S-1-15-2-1`) |

Grants are idempotent. `SetEntriesInAclW` merges ACEs, so repeated grants produce identical DACLs. After the first successful spawn, all ancestors have the ACE and subsequent spawns skip the grant.

### Security impact

Minimal. `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES` with `NO_INHERITANCE` reveals only that a directory exists (information already available via `SeChangeNotifyPrivilege`). No read of directory contents. No propagation to children.

---

## Windows: NUL Device Access

AppContainer processes cannot open `\\.\NUL` by default. This breaks any child process using `Stdio::null()`. A [known Windows limitation](https://github.com/microsoft/win32-app-isolation/issues/73) with no built-in fix.

**Solution:** One-time DACL modification on `\\.\NUL` to grant ALL APPLICATION PACKAGES (`S-1-15-2-1`) read/write access. Requires elevation. Persistent across reboots, affects all AppContainer processes. Handled by `grant_appcontainer_prerequisites()` / `lot setup`.

---

## Windows: Win32 App Isolation (Future)

Win32 App Isolation (Windows 11 24H2+) builds on AppContainer with a Brokering File System (BFS) that mediates file access without DACL manipulation. This would eliminate lot's ACL grant/revoke/sentinel complexity.

### Why lot cannot adopt it yet

- **No arbitrary path grants in the manifest.** Lot needs caller-specified paths; the capability model only provides predefined categories.
- **BFS policy API is undocumented.** The IOCTLs exist but were discovered via CVE reverse engineering, not official documentation.
- **Windows 11 24H2+ only** — drops Windows 10 support.
- **MSIX registration latency** — seconds, not milliseconds.
- **Still in preview** — Microsoft disclaims API stability.

### What would unblock adoption

1. Documented BFS policy API for programmatic path authorization.
2. Manifest support for arbitrary path declarations.
3. A programmatic API to grant file access to an AppSilo process without modifying filesystem DACLs.

---

## Deny Paths

Deny paths carve out subtrees from granted paths, blocking all access (read, write, execute) to the denied directory and everything beneath it. Each deny path must be a strict child of at least one grant path — a deny with no enclosing grant is rejected as `InvalidPolicy` (already denied by default). An exact match between a deny and grant path is also rejected (remove the grant instead). No grant path may be nested under a deny path (unreachable grant). Deny paths must not overlap with each other.

### Cross-platform behavior

| Aspect | Linux | macOS | Windows |
|---|---|---|---|
| Denied subtree visible in parent `readdir`? | Yes (empty dir) | Yes | Yes |
| `stat()` on denied path succeeds? | Yes (empty tmpfs) | No (EPERM) | No (ACCESS_DENIED) |
| Read/write/create inside denied path | Fails (ENOENT/EROFS) | Fails (EPERM) | Fails (ACCESS_DENIED) |

The `stat()` inconsistency on Linux is accepted: the security guarantee (no file access inside denied subtrees) holds on all platforms.

---

## Graceful Degradation

Lot does not silently degrade. If a required mechanism is unavailable, `spawn()` returns `SandboxError::Setup` with a diagnostic message.

| Situation | Behavior |
|---|---|
| Linux: user namespaces disabled | `SandboxError::Setup` with actionable message |
| Linux: cgroups v2 not mounted or not delegated | `SandboxError::Setup` with diagnostic (how to enable delegation) |
| Linux: seccomp not available | `SandboxError::Setup` — namespaces without seccomp is too weak |
| macOS: `sandbox_init` fails | `SandboxError::Setup` — no fallback |
| Windows: AppContainer profile creation fails | `SandboxError::Setup` |
| Windows: ACL cleanup fails on drop | Logged, not panic. Stale sentinel enables recovery via `cleanup_stale()` |

---

## Testing Strategy

### Unit tests
- Policy validation (invalid paths, empty policy, conflicting paths).
- Seccomp filter construction (syscall allowlist correctness).
- Seatbelt profile generation (SBPL string correctness).
- ACL grant/revoke logic (Windows).

### Integration tests (platform-specific, require real OS mechanisms)
- Spawn sandboxed process, verify it can read allowed paths.
- Verify it cannot read disallowed paths or write to read-only paths.
- Deny paths: verify denied subtrees block reads while sibling paths remain accessible.
- Network denied: verify `connect()` fails.
- Memory limit: child exceeds limit, gets killed.
- Process limit: child fork-bombs, limited by cgroup/job object.
- Cleanup: after `SandboxedChild` drops, ACLs/cgroups/profiles are removed.

Tests gate on `probe()` results at runtime, skipping when a mechanism is unavailable rather than failing.

### CI

| Job | Platform | Setup |
|---|---|---|
| Format | ubuntu-latest | `cargo fmt --all --check` |
| Clippy | ubuntu-latest, macos-latest, windows-latest (matrix) | `cargo clippy --workspace --all-targets --features tokio` |
| Build | ubuntu-latest | `cargo build --workspace` |
| Test (Linux) | ubuntu-latest | `sysctl apparmor_restrict_unprivileged_userns=0`, cgroup delegation |
| Test (macOS) | macos-latest | None needed |
| Test (Windows) | windows-latest | `lot setup` for AppContainer prerequisites |

Tests run with `--test-threads=1` (integration tests modify system state).
