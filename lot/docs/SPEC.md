# Lot — Cross-Platform Process Sandboxing for Rust

Standalone library crate for launching sandboxed child processes. Restricts filesystem, network, and resource access using native OS mechanisms.

## Goals

1. Launch a child process with a declarative policy (allowed paths, network, resource limits).
2. Kernel-enforced isolation — no in-process hooking, no TOCTOU races.
3. Cross-platform: Linux, macOS, Windows.
4. Usable from async runtimes (tokio) — no thread restrictions on the caller.
5. Simple API: build policy, spawn child, collect output.
6. Async where the OS supports it natively (e.g., `pidfd` on Linux, overlapped I/O on Windows). Blocking where async has no OS support — do not spawn threads just to make the API async.

## Non-Goals

- Sandboxing the calling process.
- GPU passthrough or device-specific policy (out of scope for v1).
- Container orchestration (Docker, Podman, etc.).
- Privileged operation — must work without root/admin.

---

## Platform Mechanisms

### Linux: Namespaces + seccomp-BPF + cgroups v2

**Namespaces** provide the primary isolation boundary:

| Namespace | Purpose |
|---|---|
| `CLONE_NEWUSER` | Unprivileged namespace creation. Maps caller UID/GID into child. |
| `CLONE_NEWNS` | Mount namespace. Private root with only allowed paths bind-mounted. |
| `CLONE_NEWPID` | PID namespace. Child sees itself as PID 1. Cannot signal host processes. |
| `CLONE_NEWNET` | Network namespace. Empty network stack by default (no interfaces). Optional: create veth pair for controlled access. |
| `CLONE_NEWIPC` | IPC namespace. Isolates SysV IPC and POSIX message queues. |

**seccomp-BPF** filters syscalls:
- Allowlist of permitted syscalls.
- Default action: `EPERM` (not `SIGKILL` — debuggable).
- Conditional rules: network syscalls gated on policy, `ioctl` restricted.

**cgroups v2** enforces resource limits:
- Memory limit (`memory.max`).
- CPU bandwidth (`cpu.max` — period/quota).
- PID limit (`pids.max` — prevent fork bombs).
- Cgroup created per sandbox invocation, cleaned up on drop.

**Filesystem setup:**
1. Create tmpfs at a temporary mount point.
2. Bind-mount allowed read-only paths with `MS_RDONLY | MS_NOSUID | MS_NODEV`.
3. Bind-mount allowed read-write paths with `MS_NOSUID | MS_NODEV`.
4. Bind-mount allowed executable paths with `MS_RDONLY | MS_NOSUID | MS_NODEV` (no `MS_NOEXEC`).
5. `pivot_root` into the new root. Unmount old root.
6. Essential paths (`/proc`, `/dev/null`, `/dev/urandom`, dynamic linker paths) are always mounted read-only.

**Single-threaded constraint:** `CLONE_NEWUSER` requires the calling process to be single-threaded. Lot handles this internally by forking a single-threaded helper process before calling `clone()` with namespace flags. The caller (which may be multi-threaded/tokio) never directly enters the namespace.

### macOS: Seatbelt (sandbox_init)

**Seatbelt** is macOS's kernel-level sandbox, configured via SBPL (Sandbox Profile Language) profiles.

FFI surface is two functions:
```c
int sandbox_init(const char *profile, uint64_t flags, char **errorbuf);
void sandbox_free_error(char *errorbuf);
```

**Profile generation:**
- Start with `(version 1) (deny default)`.
- Add `(allow file-read* (subpath "..."))` for read-only paths.
- Add `(allow file-read* (subpath "...")) (allow file-write* (subpath "..."))` for read-write paths.
- Add `(allow process-exec (subpath "..."))` for executable paths.
- Add `(allow network-outbound) (allow network-inbound)` if network permitted.
- Always allow: system libraries (`/usr/lib`, `/System/Library`), dynamic linker cache, `/dev/urandom`.

**Process model:** Fork a helper, call `setsid()` so the child becomes its own process group leader (enabling `killpg` to kill all descendants on drop/timeout), apply `sandbox_init` in the helper (permanent, no undo), then exec the target command. The parent (caller) is never sandboxed.

**Resource limits:** `setrlimit(RLIMIT_AS, ...)` for memory. No cgroup equivalent on macOS — rlimit is the available mechanism.

**Deprecation note:** Apple deprecated `sandbox_init` but has not removed it. It is still used by major applications (Chrome, Firefox) and the underlying kernel sandbox is actively maintained. No replacement API exists for third-party use.

### Windows: AppContainer + Job Objects

**AppContainer** is a Windows kernel security boundary that denies access to all resources by default. Access is granted explicitly via:
- ACL entries on files/directories/registry keys, granting the AppContainer's package SID read or read-write access.
- Capability SIDs (e.g., `InternetClient` for network access).

**Process model:**
1. Create or open an AppContainer profile (`CreateAppContainerProfile`). Derives a unique package SID.
2. Build a `SECURITY_CAPABILITIES` structure with the package SID and desired capability SIDs.
3. Call `CreateProcessW` with `STARTUPINFOEX` containing the security capabilities via `UpdateProcThreadAttribute`.
4. The child process runs inside the AppContainer boundary.

**Filesystem access:**
- Before launch, grant the package SID read or read-write ACL entries on allowed paths (`SetEntriesInAcl`, `SetNamedSecurityInfo`).
- On cleanup, remove the ACL entries.

**Network access:**
- Denied by default inside AppContainer.
- Granted by adding `InternetClient` or `InternetClientServer` capability SIDs.

**Job Objects** enforce resource limits:
- `JOB_OBJECT_LIMIT_PROCESS_MEMORY` — memory cap.
- `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` — limit child process count.
- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` — RAII cleanup: closing the job handle kills all processes in the job.
- UI restrictions: block clipboard, desktop, display settings access.

**Cleanup:** AppContainer profiles persist on the system. Lot must delete the profile on drop. ACL entries on project files must be restored via a sentinel file approach:
1. Before granting ACLs, write a manifest of modified paths + original DACLs to a sentinel file.
2. On normal exit, restore ACLs and delete sentinel.
3. On next `spawn()`, check for stale sentinels from crashed sessions and restore ACLs before proceeding.
4. Expose `cleanup_stale()` as a public function for callers that want explicit control.

---

## API Design

### Policy

```rust
/// What a sandboxed process is allowed to do.
pub struct SandboxPolicy {
    /// Paths the child can read (recursive).
    pub read_paths: Vec<PathBuf>,
    /// Paths the child can read and write (recursive).
    pub write_paths: Vec<PathBuf>,
    /// Paths the child can execute from (recursive).
    /// Typically: /bin, /usr/bin, project tool paths.
    pub exec_paths: Vec<PathBuf>,
    /// Allow outbound network access (boolean for v1).
    /// Future: granular allowlist would need DNS names, not just IP/port.
    pub allow_network: bool,
    /// Resource limits.
    pub limits: ResourceLimits,
}

pub struct ResourceLimits {
    /// Maximum memory in bytes. None = no limit.
    pub max_memory_bytes: Option<u64>,
    /// Maximum number of child processes. None = no limit.
    pub max_processes: Option<u32>,
    /// Maximum CPU time in seconds. None = no limit.
    pub max_cpu_seconds: Option<u64>,
}
```

### Spawning

```rust
/// A command to run inside a sandbox.
pub struct SandboxCommand {
    program: OsString,
    args: Vec<OsString>,
    /// Additional env vars to set in the child. Default environment is minimal:
    /// only platform essentials (e.g., SystemRoot/TEMP on Windows, PATH
    /// pointing to allowed exec paths on Unix). No wholesale inheritance.
    /// To forward a parent var, read it from std::env and pass it here,
    /// or call `forward_common_env()` to forward a standard set.
    env: Vec<(OsString, OsString)>,
    cwd: Option<PathBuf>,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
}

/// A running sandboxed process.
pub struct SandboxedChild {
    pid: u32,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    // Platform-specific cleanup handle (job object, cgroup path, etc.)
    guard: SandboxGuard,
}

impl SandboxedChild {
    pub fn id(&self) -> u32;
    pub fn kill(&mut self) -> io::Result<()>;
    pub fn wait(&mut self) -> io::Result<ExitStatus>;
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    pub fn wait_with_output(self) -> io::Result<Output>;
    pub fn take_stdin(&mut self) -> Option<ChildStdin>;
    pub fn take_stdout(&mut self) -> Option<ChildStdout>;
    pub fn take_stderr(&mut self) -> Option<ChildStderr>;

    /// Kill the sandboxed process and all descendants, then run platform
    /// cleanup synchronously. Consumes self so Drop does not re-run.
    pub fn kill_and_cleanup(self) -> Result<(), SandboxError>;

    /// (Requires `tokio` feature) Wait for exit with a timeout. On
    /// timeout, kills all descendants, runs cleanup, returns
    /// SandboxError::Timeout.
    #[cfg(feature = "tokio")]
    pub async fn wait_with_output_timeout(
        self,
        timeout: Duration,
    ) -> Result<Output, SandboxError>;
}
```

### SandboxCommand convenience methods

```rust
impl SandboxCommand {
    /// Forward a standard set of environment variables from the parent
    /// process (PATH, HOME, USER, LANG, TMPDIR, SYSTEMROOT, etc.).
    /// Missing keys are silently skipped.
    pub fn forward_common_env(&mut self) -> &mut Self;
}
```

### Policy builder

```rust
/// Ergonomic builder with auto-canonicalization, deduplication, and
/// platform defaults. Produces a validated SandboxPolicy.
let policy = SandboxPolicyBuilder::new()
    .read_path("/project")              // auto-canonicalized; skipped if non-existent
    .write_path("/project/src")         // deduped against read_paths
    .include_temp_dirs()                // platform temp dir → write_paths
    .include_platform_exec_paths()      // /usr/bin, System32, etc.
    .include_platform_lib_paths()       // /usr/lib, /usr/include, etc.
    .allow_network(true)
    .max_memory_bytes(512 * 1024 * 1024)
    .build()?;
```

### Entry point

```rust
/// Spawn a sandboxed child process.
///
/// The caller is never sandboxed. The child process inherits
/// the sandbox restrictions and cannot escape them.
pub fn spawn(policy: &SandboxPolicy, command: SandboxCommand) -> Result<SandboxedChild>;
```

### Error types

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    #[error("platform not supported: {0}")]
    Unsupported(String),

    #[error("sandbox setup failed: {0}")]
    Setup(String),

    #[error("policy invalid: {0}")]
    InvalidPolicy(String),

    #[error("cleanup failed: {0}")]
    Cleanup(String),

    #[error("child process timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

---

## Graceful Degradation

Not all mechanisms are available on all kernel versions or configurations.

| Situation | Behavior |
|---|---|
| Linux: user namespaces disabled (`/proc/sys/kernel/unprivileged_userns_clone = 0`) | Return `SandboxError::Setup` with actionable message. |
| Linux: cgroups v2 not mounted or not delegated | `probe()` reports `cgroups_v2: false`. `spawn()` returns `SandboxError::Setup` with diagnostic message (how to enable delegation or configure systemd user slice). Caller decides: exit with message, or disable resource-limit policy and retry. Lot does not silently degrade. |
| Linux: seccomp not available | Return `SandboxError::Setup`. Namespaces without seccomp is too weak. |
| macOS: `sandbox_init` fails | Return `SandboxError::Setup`. No fallback — Seatbelt is the only mechanism. |
| Windows: AppContainer profile creation fails | Return `SandboxError::Setup`. |
| Windows: ACL cleanup fails on drop | Log error. Do not panic. Stale sentinel enables recovery on next `spawn()` or via `cleanup_stale()`. |

A `probe()` function reports what mechanisms are available:

```rust
pub struct PlatformCapabilities {
    pub namespaces: bool,      // Linux
    pub seccomp: bool,         // Linux
    pub cgroups_v2: bool,      // Linux
    pub seatbelt: bool,        // macOS
    pub appcontainer: bool,    // Windows
    pub job_objects: bool,     // Windows
}

/// Check what sandboxing mechanisms are available on the current platform.
pub fn probe() -> PlatformCapabilities;

/// Restore ACLs from any stale sentinel files left by crashed sessions (Windows).
/// No-op on other platforms.
pub fn cleanup_stale() -> Result<()>;
```

---

## Internal Architecture

```
lot/
  src/
    lib.rs            — Public API: spawn(), probe(), types
    policy.rs         — SandboxPolicy, ResourceLimits, validation
    policy_builder.rs — SandboxPolicyBuilder (auto-canonicalization, platform defaults)
    command.rs        — SandboxCommand builder
    error.rs          — SandboxError
    linux/
      mod.rs        — LinuxSandbox: orchestrates namespace + seccomp + cgroup
      namespace.rs  — clone(), pivot_root, bind mounts, uid/gid mapping
      seccomp.rs    — BPF filter construction and application
      cgroup.rs     — cgroup v2 creation, limit writes, cleanup
    macos/
      mod.rs        — MacSandbox: fork + seatbelt + rlimit
      seatbelt.rs   — SBPL profile generation, sandbox_init FFI
    windows/
      mod.rs        — WindowsSandbox: AppContainer + job object
      appcontainer.rs — Profile lifecycle, capability assembly, ACL management
      job.rs        — Job object creation, resource limits
```

---

## Dependencies

| Crate | Platform | Purpose |
|---|---|---|
| `libc` | Linux, macOS | Syscall wrappers (`clone`, `setrlimit`, `prctl`, etc.) |
| `seccompiler` | Linux | seccomp-BPF filter construction |
| `windows-sys` | Windows | Win32 API bindings (AppContainer, Job Objects, ACL) |
| `thiserror` | All | Error derive |
| `tracing` | All | Structured logging (optional feature) |
| `tokio` | All | Async runtime for `wait_with_output_timeout` (optional `tokio` feature) |

No dependency on `rappct`, `birdcage`, or `yule-sandbox`. All platform code is written from scratch.

---

## Project Setup

```toml
[package]
name = "lot"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "MIT"

[dependencies]
thiserror = "2"

[target.'cfg(target_os = "linux")'.dependencies]
libc = "0.2"
seccompiler = "0.5"

[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"

[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Security",
    "Win32_Security_Authorization",
    "Win32_System_JobObjects",
    "Win32_System_Threading",
    "Win32_Foundation",
] }

[features]
tokio = ["dep:tokio"]
tracing = ["dep:tracing"]

[dependencies.tokio]
version = "1"
optional = true
features = ["rt", "time", "sync", "macros"]

[dependencies.tracing]
version = "0.1"
optional = true

[lints.rust]
unsafe_code = "warn"  # Required for syscalls, but flagged for review

[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }
missing_errors_doc = "allow"
missing_panics_doc = "allow"
module_name_repetitions = "allow"
must_use_candidate = "allow"
```

Note: `unsafe_code = "warn"` rather than `"deny"` — this crate necessarily uses unsafe for syscalls, namespace setup, and FFI. Each `unsafe` block must have a `// SAFETY:` comment.

---

## Testing Strategy

### Unit tests
- Policy validation (invalid paths, empty policy, conflicting paths).
- Seccomp filter construction (syscall allowlist correctness).
- Seatbelt profile generation (SBPL string correctness).
- ACL grant/revoke logic (Windows).

### Integration tests (platform-specific, require real OS mechanisms)
- Spawn sandboxed process, verify it can read allowed paths.
- Spawn sandboxed process, verify it cannot read disallowed paths.
- Spawn sandboxed process, verify it cannot write to read-only paths.
- Spawn sandboxed process with network denied, verify `connect()` fails.
- Memory limit enforcement: child exceeds limit, gets killed.
- Process limit enforcement: child fork-bombs, limited by cgroup/job object.
- Cleanup verification: after `SandboxedChild` drops, ACLs/cgroups/profiles are removed.

### CI
- Linux tests on GitHub Actions (Ubuntu, kernel 5.15+).
- macOS tests on GitHub Actions (macOS runner).
- Windows tests on GitHub Actions (Windows runner).
- Each platform runs only its own integration tests.

