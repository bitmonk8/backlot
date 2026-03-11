# lot

Cross-platform process sandboxing for Rust. Launch child processes with restricted filesystem, network, and resource access using native OS mechanisms.

## Platform Mechanisms

| Platform | Isolation | Resource Limits |
|----------|-----------|-----------------|
| Linux | User/mount/PID/net/IPC namespaces + seccomp-BPF | cgroups v2 |
| macOS | Seatbelt (`sandbox_init` SBPL profiles) | `setrlimit` |
| Windows | AppContainer + ACLs | Job Objects |

All enforcement is kernel-level. No in-process hooking, no TOCTOU races. Only the child process is sandboxed — the caller is never restricted. Works without root/admin.

## Usage

Add to `Cargo.toml`:

```toml
[dependencies]
lot = "0.1"
```

For async timeout support (`wait_with_output_timeout`):

```toml
[dependencies]
lot = { version = "0.1", features = ["tokio"] }
```

### Basic Example

```rust
use lot::{SandboxPolicy, SandboxCommand, ResourceLimits, spawn, probe};
use std::path::PathBuf;

// Check available mechanisms
let caps = probe();
println!("{caps:?}");

// Define policy
let policy = SandboxPolicy {
    read_paths: vec![PathBuf::from("/usr/lib")],
    write_paths: vec![],
    exec_paths: vec![PathBuf::from("/usr/bin")],
    allow_network: false,
    limits: ResourceLimits {
        max_memory_bytes: Some(64 * 1024 * 1024), // 64 MB
        max_processes: Some(4),
        max_cpu_seconds: None,
    },
};

// Build command
let mut cmd = SandboxCommand::new("/usr/bin/echo");
cmd.arg("hello from sandbox");

// Spawn and collect output
let child = spawn(&policy, &cmd).expect("spawn failed");
let output = child.wait_with_output().expect("wait failed");
println!("{}", String::from_utf8_lossy(&output.stdout));
```

## API

### `probe() -> PlatformCapabilities`

Reports which sandboxing mechanisms are available on the current platform.

```rust
pub struct PlatformCapabilities {
    pub namespaces: bool,    // Linux
    pub seccomp: bool,       // Linux
    pub cgroups_v2: bool,    // Linux
    pub seatbelt: bool,      // macOS
    pub appcontainer: bool,  // Windows
    pub job_objects: bool,   // Windows
}
```

### `spawn(policy, command) -> Result<SandboxedChild>`

Launches a sandboxed child process. Validates the policy before spawning.

### `cleanup_stale() -> Result<()>`

Restores ACLs from sentinel files left by crashed sessions. Windows only; no-op on other platforms.

### `SandboxPolicy`

```rust
pub struct SandboxPolicy {
    pub read_paths: Vec<PathBuf>,    // Recursive read access
    pub write_paths: Vec<PathBuf>,   // Recursive read+write access
    pub exec_paths: Vec<PathBuf>,    // Recursive execute access
    pub allow_network: bool,         // Outbound network access
    pub limits: ResourceLimits,
}
```

Paths must exist. No overlaps allowed within or across path lists. At least one path must be specified.

### `SandboxPolicyBuilder`

Ergonomic builder with auto-canonicalization, overlap deduction, and platform defaults:

```rust
use lot::SandboxPolicyBuilder;

let policy = SandboxPolicyBuilder::new()
    .read_path("/project")              // auto-canonicalized; skipped if non-existent
    .write_path("/project/src")         // deduped against read_paths
    .include_temp_dirs()                // platform temp dir → write_paths
    .include_platform_exec_paths()      // /usr/bin, System32, etc.
    .include_platform_lib_paths()       // /usr/lib, /usr/include, etc.
    .allow_network(false)
    .max_memory_bytes(128 * 1024 * 1024)
    .max_processes(8)
    .build()?;
```

Non-existent paths are silently skipped. Narrower paths subsumed by broader entries are deduplicated. Produces a validated `SandboxPolicy`.

### `ResourceLimits`

```rust
pub struct ResourceLimits {
    pub max_memory_bytes: Option<u64>,   // None = no limit
    pub max_processes: Option<u32>,      // None = no limit
    pub max_cpu_seconds: Option<u64>,    // None = no limit
}
```

### `SandboxCommand`

Builder for the child process command. Methods chain via `&mut Self`:

- `new(program)` — program to execute
- `arg(s)` / `args(iter)` — arguments
- `env(key, val)` — additional environment variable
- `forward_common_env()` — forward standard env vars (PATH, HOME, TEMP, SYSTEMROOT, etc.) from parent; missing keys silently skipped
- `cwd(path)` — working directory
- `stdin(stdio)` / `stdout(stdio)` / `stderr(stdio)` — I/O configuration

Defaults: stdin `Null`, stdout `Piped`, stderr `Piped`.

Environment is minimal by default (platform essentials only). Pass variables explicitly via `env()`, or use `forward_common_env()` to forward a standard set.

### `SandboxStdio`

```rust
pub enum SandboxStdio {
    Null,    // /dev/null (NUL on Windows)
    Piped,   // Anonymous pipe to parent
    Inherit, // Inherit parent's handle
}
```

### `SandboxedChild`

Handle to a running sandboxed process:

- `id() -> u32` — OS process ID
- `kill() -> io::Result<()>` — forcibly terminate
- `wait() -> io::Result<ExitStatus>` — block until exit
- `try_wait() -> io::Result<Option<ExitStatus>>` — non-blocking poll
- `wait_with_output(self) -> io::Result<Output>` — wait and collect stdout/stderr
- `kill_and_cleanup(self) -> Result<()>` — kill all descendants, run platform cleanup synchronously, consume self
- `wait_with_output_timeout(self, timeout) -> Result<Output>` — *(requires `tokio` feature)* async wait with timeout; kills and cleans up on timeout
- `take_stdin()` / `take_stdout()` / `take_stderr()` — take piped I/O handles

Dropping the handle performs platform-specific cleanup (ACL restoration, cgroup removal, etc.). On macOS, drop kills the entire process group (descendants included).

### `SandboxError`

```rust
pub enum SandboxError {
    Unsupported(String),        // Platform lacks required mechanism
    Setup(String),              // OS-level sandbox setup failed
    InvalidPolicy(String),      // Policy validation failed
    Cleanup(String),            // Post-session cleanup failed
    Timeout(Duration),          // Child exceeded timeout (from wait_with_output_timeout)
    Io(std::io::Error),         // Underlying I/O error
}
```

## Graceful Degradation

Lot does not silently degrade. If a required mechanism is unavailable, `spawn()` returns `SandboxError::Setup` with a diagnostic message. Use `probe()` to check capabilities before spawning.

| Situation | Behavior |
|-----------|----------|
| Linux: user namespaces disabled | `SandboxError::Setup` |
| Linux: cgroups v2 not delegated | `SandboxError::Setup` with instructions |
| Linux: seccomp unavailable | `SandboxError::Setup` |
| macOS: `sandbox_init` fails | `SandboxError::Setup` |
| Windows: AppContainer creation fails | `SandboxError::Setup` |
| Windows: ACL cleanup fails on drop | Logged, recoverable via `cleanup_stale()` |

## Windows: NUL Device Access

AppContainer-sandboxed processes cannot open the Windows NUL device (`\\.\NUL`) by default. All path variants return `ERROR_ACCESS_DENIED`. This breaks any child process that uses `Stdio::null()` — Rust's standard library opens `\\.\NUL` internally, and tools like Nushell set `stdin(Stdio::null())` for external commands.

This is a [known Windows limitation](https://github.com/microsoft/win32-app-isolation/issues/73) with no built-in fix from Microsoft.

### Fix

A one-time DACL modification grants `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) read/write access to the NUL device. This requires elevation (run as administrator) and persists across reboots. It does not weaken AppContainer isolation — NUL is a data sink, not a privilege escalation vector.

Three Windows-only functions:

- `nul_device_accessible() -> bool` — checks if AppContainer processes can access `\\.\NUL`
- `can_modify_nul_device() -> bool` — checks if the current process is elevated (can modify the DACL)
- `grant_nul_device_access() -> Result<()>` — grants access (idempotent)

```rust
use lot::{nul_device_accessible, can_modify_nul_device, grant_nul_device_access};

if !nul_device_accessible() {
    if can_modify_nul_device() {
        grant_nul_device_access().expect("failed to grant NUL device access");
    } else {
        eprintln!("Run as administrator to fix NUL device access for AppContainer sandboxes");
    }
}
```

## Known Limitations

- `max_cpu_seconds` is not enforced on Linux (cgroups v2 `cpu.max` controls bandwidth, not total time). Enforced on Windows and macOS.
- macOS `mach-lookup` is unrestricted in Seatbelt profiles (restricting it breaks most programs).
- Linux namespace tests require `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+.
- Windows: AppContainer processes cannot access `\\.\NUL` without a one-time system fix (see above).

## Requirements

- Rust 1.85+ (edition 2024)
- Linux: kernel 5.15+, unprivileged user namespaces enabled, cgroups v2 with user delegation (for resource limits)
- macOS: Seatbelt support (all recent macOS versions)
- Windows: Windows 10+ (AppContainer support)

## License

MIT
