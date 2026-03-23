# lot

Cross-platform process sandboxing for Rust. Launch child processes with restricted filesystem, network, and resource access using native OS mechanisms.

[![crates.io](https://img.shields.io/crates/v/lot.svg)](https://crates.io/crates/lot)
[![docs.rs](https://docs.rs/lot/badge.svg)](https://docs.rs/lot)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

## Platform Mechanisms

| Platform | Isolation | Resource Limits |
|----------|-----------|-----------------|
| Linux | User/mount/PID/net/IPC namespaces + seccomp-BPF | cgroups v2 |
| macOS | Seatbelt (`sandbox_init` SBPL profiles) | `setrlimit` |
| Windows | AppContainer + ACLs | Job Objects |

All enforcement is kernel-level. No in-process hooking, no TOCTOU races. Only the child process is sandboxed -- the caller is never restricted. Works without root/admin.

## Quick Start

Add to `Cargo.toml`:

```toml
[dependencies]
lot = "0.1"
```

```rust
use lot::{SandboxPolicyBuilder, SandboxCommand, spawn};

let policy = SandboxPolicyBuilder::new()
    .include_platform_exec_paths().expect("exec paths")
    .include_platform_lib_paths().expect("lib paths")
    .allow_network(false)
    .max_memory_bytes(64 * 1024 * 1024) // 64 MB
    .build()
    .expect("policy invalid");

let mut cmd = SandboxCommand::new("/bin/echo");
cmd.arg("hello from sandbox");

let child = spawn(&policy, &cmd).expect("spawn failed");
let output = child.wait_with_output().expect("wait failed");
println!("{}", String::from_utf8_lossy(&output.stdout));
```

## Feature Flags

| Flag | Effect |
|------|--------|
| `tokio` | Enables `SandboxedChild::wait_with_output_timeout` for async wait with timeout. |

```toml
[dependencies]
lot = { version = "0.1", features = ["tokio"] }
```

## API Reference

Full API documentation is available on [docs.rs](https://docs.rs/lot).

## CLI

Install:

```bash
cargo install --path lot-cli
```

### `lot run`

Run a program inside a sandbox defined by a YAML config file.

```bash
lot run --config sandbox.yaml -- ./my-program arg1 arg2
lot run -c sandbox.yaml -t 30 -- ./long-task          # 30s timeout
lot run -c sandbox.yaml --dry-run                      # validate only
lot run -c sandbox.yaml --verbose -- ./my-program      # verbose output
```

Exit code: forwards the child's exit code. Timeout exits 124 (GNU `timeout` convention). Setup failure exits 1.

Stdio is inherited by default -- the sandboxed process reads/writes the terminal directly.

#### Config file format

```yaml
# Filesystem access -- all paths are auto-canonicalized.
# Non-existent paths are skipped with a warning (--verbose).
filesystem:
  read:
    - /usr/lib
    - /project/data
  write:
    - /tmp/output
  exec:
    - /usr/bin
    - /project/bin
  deny:
    - /project/data/secrets      # block access to subtree within granted paths
  include_platform_exec: true    # /usr/bin, /bin, System32, etc.
  include_platform_lib: true     # /usr/lib, /usr/include, Framework dirs, etc.
  include_temp: true             # Platform temp directory -> write_paths

# Network access. Default: false (denied).
network:
  allow: false

# Resource limits. All optional -- omitted = no limit.
limits:
  max_memory_bytes: 536870912    # 512 MB
  max_processes: 10
  max_cpu_seconds: 60

# Environment variables for the child process.
environment:
  forward_common: true           # Forward PATH, HOME, USER, LANG, etc.
  vars:
    RUST_LOG: debug
    MY_VAR: value

# Working directory for the child. Optional -- defaults to "/".
process:
  cwd: /project
```

All sections are optional except that at least one grant path (read, write, or exec) is required. An empty policy with no paths is rejected by `validate()`. A minimal config with only system paths (via `include_platform_exec: true`) is valid.

### `lot setup`

Configure platform prerequisites. Windows only (no-op on other platforms).

```bash
lot setup --verbose       # grant prerequisites (requires elevation)
lot setup --check         # check without modifying
```

### `lot probe`

Print platform sandboxing capabilities.

```bash
lot probe
```

Output:
```
appcontainer=true
job_objects=true
namespaces=false
seccomp=false
cgroups_v2=false
seatbelt=false
```

## Windows: AppContainer Prerequisites

AppContainer-sandboxed processes cannot open the Windows NUL device (`\\.\NUL`) by default, and cannot traverse ancestor directories of policy paths without explicit ACEs. This requires a one-time setup step.

This is a [known Windows limitation](https://github.com/microsoft/win32-app-isolation/issues/73) with no built-in fix from Microsoft.

### Setup

Use the CLI or the library API to grant prerequisites. Requires elevation (run as administrator). The changes persist across reboots and do not weaken AppContainer isolation.

**CLI:**

```bash
lot setup --verbose       # grant prerequisites
lot setup --check         # check without modifying
```

**Library API (Windows-only):**

- `is_elevated() -> bool` -- checks if the current process has administrator privileges
- `grant_appcontainer_prerequisites_for_policy(policy) -> Result<()>` -- grants NUL device access and ancestor traverse ACEs for all policy paths
- `appcontainer_prerequisites_met_for_policy(policy) -> bool` -- checks if prerequisites are already in place

For user-owned directories, `spawn()` grants ancestor traverse ACEs automatically at spawn time. The setup step is only needed for the NUL device and system directories.

## Known Limitations

- `max_cpu_seconds` is not enforced on Linux (cgroups v2 `cpu.max` controls bandwidth, not total time). Enforced on Windows and macOS.
- macOS `mach-lookup` is unrestricted in Seatbelt profiles (restricting it breaks most programs).
- Linux namespace tests require `kernel.apparmor_restrict_unprivileged_userns=0` on Ubuntu 24.04+.
- Windows: AppContainer processes cannot access `\\.\NUL` without a one-time system fix (see above).
- Deny paths: `stat()` succeeds on denied paths on Linux (shows empty tmpfs metadata) but fails on macOS/Windows. File access is blocked on all platforms.
- Linux kernels < 5.9: parallel `spawn()` calls from multi-threaded processes may hit ETXTBSY due to missing `close_range` syscall. Works correctly on 5.9+.

## Requirements

- Rust 1.85+ (edition 2024)
- Linux: kernel 5.15+, unprivileged user namespaces enabled, cgroups v2 with user delegation (for resource limits)
- macOS: Seatbelt support (all recent macOS versions)
- Windows: Windows 10+ (AppContainer support)

## License

MIT OR Apache-2.0 ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE))
