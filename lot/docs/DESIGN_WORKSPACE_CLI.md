# Design: Workspace Restructure + CLI Binary

## Motivation

lot needs a way to invoke `grant_appcontainer_prerequisites()` (and future platform setup) from CI and end users. The sibling projects reel and flick use a workspace pattern: library crate + CLI crate. lot should follow the same pattern, becoming "dual nature" (library + CLI).

Immediate driver: Windows CI fails because no setup step calls `grant_appcontainer_prerequisites()`. A `lot setup` command solves this.

## Target Structure

```
lot/                           (workspace root)
├── Cargo.toml                 (workspace config, shared lints/versions/profile)
├── lot/                       (library crate — existing code moves here)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── policy.rs
│       ├── policy_builder.rs
│       ├── command.rs
│       ├── error.rs
│       ├── linux/
│       ├── macos/
│       └── windows/
├── lot-cli/                   (CLI binary crate)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
├── tests/                     (integration tests — stay at workspace root)
│   └── integration.rs
├── docs/
├── prompts/
└── .github/
```

## Workspace Root Cargo.toml

```toml
[workspace]
members = ["lot", "lot-cli"]
resolver = "3"

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"
rust-version = "1.85"

[workspace.lints.rust]
unsafe_code = "warn"
warnings = "deny"
missing_docs = "warn"

[workspace.lints.clippy]
# Same as current lot/Cargo.toml [lints.clippy] section

[profile.release]
lto = true
codegen-units = 1
strip = true
```

## Library Crate (lot/Cargo.toml)

Same dependencies as the current Cargo.toml, but metadata fields reference workspace:

```toml
[package]
name = "lot"
description = "Cross-platform process sandboxing: seccomp, AppContainer, Seatbelt"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
thiserror = "2"

# ... all existing target-specific dependencies unchanged ...

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

## CLI Crate (lot-cli/Cargo.toml)

```toml
[package]
name = "lot-cli"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[[bin]]
name = "lot"
path = "src/main.rs"

[lints]
workspace = true

[dependencies]
lot = { path = "../lot", features = ["tokio"] }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_yml = "0.0.12"
tokio = { version = "1", features = ["rt", "time", "macros"] }
```

Tokio is included for `--timeout` support via `wait_with_output_timeout()`. The runtime is only constructed when `--timeout` is specified. Setup and probe remain synchronous code paths.

## CLI Commands

### `lot run`

Runs a program inside a sandbox. The sandbox policy is defined in a YAML config file.

```
lot run --config <path> [--timeout <secs>] [--verbose] [--dry-run] -- <program> [args...]
```

| Flag | Behavior |
|---|---|
| `--config` / `-c` | Path to YAML sandbox config file. Required. |
| `--timeout` / `-t` | Wall-clock timeout in seconds. Kills sandbox on expiry. Optional. |
| `--verbose` / `-v` | Print sandbox setup details to stderr. |
| `--dry-run` | Validate config, print effective policy to stderr, exit without spawning. |

**Program and arguments** come after `--` to avoid ambiguity with lot's own flags.

**Stdio**: Inherited by default — the sandboxed process reads/writes the terminal directly.

**Exit code**: Forwards the child's exit code. If the child is killed by timeout, exits 124 (matching GNU `timeout` convention). If sandbox setup fails, exits 1.

**Environment**: By default, the child gets a minimal environment (platform essentials only). The config file controls additional env vars.

#### Config file format (YAML)

```yaml
# Filesystem access — all paths are auto-canonicalized.
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
  # Convenience: include platform defaults. Default: false.
  include_platform_exec: true    # /usr/bin, /bin, System32, etc.
  include_platform_lib: true     # /usr/lib, /usr/include, Framework dirs, etc.
  include_temp: true             # Platform temp directory → write_paths

# Network access. Default: false (denied).
network:
  allow: false

# Resource limits. All optional — omitted = no limit.
limits:
  max_memory_bytes: 536870912    # 512 MB
  max_processes: 10
  max_cpu_seconds: 60

# Environment variables for the child process.
environment:
  # Forward standard env vars from parent (PATH, HOME, USER, LANG, etc.)
  forward_common: true
  # Additional explicit vars.
  vars:
    RUST_LOG: debug
    MY_VAR: value

# Working directory for the child. Optional — defaults to "/".
process:
  cwd: /project
```

All sections are optional. An empty config file means the child can access nothing (deny-all). This is valid but will cause most programs to fail immediately.

#### Config-to-library mapping

| Config field | Library call |
|---|---|
| `filesystem.read` | `SandboxPolicyBuilder::read_path()` for each |
| `filesystem.write` | `SandboxPolicyBuilder::write_path()` for each |
| `filesystem.exec` | `SandboxPolicyBuilder::exec_path()` for each |
| `filesystem.include_platform_exec` | `SandboxPolicyBuilder::include_platform_exec_paths()` |
| `filesystem.include_platform_lib` | `SandboxPolicyBuilder::include_platform_lib_paths()` |
| `filesystem.include_temp` | `SandboxPolicyBuilder::include_temp_dirs()` |
| `network.allow` | `SandboxPolicyBuilder::allow_network()` |
| `limits.*` | `SandboxPolicyBuilder::max_memory_bytes()`, etc. |
| `environment.forward_common` | `SandboxCommand::forward_common_env()` |
| `environment.vars` | `SandboxCommand::env()` for each |
| `process.cwd` | `SandboxCommand::cwd()` |

#### Example usage

```bash
# Run a build script in a sandbox that can read source, write to output, and access the network
lot run -c sandbox.yaml -- ./build.sh --release

# Check config validity without running
lot run -c sandbox.yaml --dry-run -- ./build.sh

# Run with 30-second wall-clock timeout
lot run -c sandbox.yaml -t 30 -- ./long-running-task
```

### `lot setup`

Configures platform prerequisites. Mirrors reel's setup command.

```
lot setup [--check] [--verbose]
```

| Flag | Behavior |
|---|---|
| (none) | Grant prerequisites. Exit 0 on success, 1 on failure. |
| `--check` | Check prerequisites without modifying. Exit 0 if OK, 1 if missing. |
| `--verbose` | Print details of what is being checked/configured. |

**Windows behavior:**
- Uses `std::env::temp_dir()` as the representative path (covers temp directory ancestors where tests create `TempDir`).
- Calls `lot::grant_appcontainer_prerequisites(&[temp_dir])` (or `lot::appcontainer_prerequisites_met()` for `--check`).

**Non-Windows behavior:**
- Prints "No setup required on this platform." and exits 0.

### `lot probe`

Prints platform capabilities as reported by `lot::probe()`.

```
lot probe
```

Output format: one capability per line, key=value. Example:

```
appcontainer=true
job_objects=true
namespaces=false
seccomp=false
cgroups_v2=false
seatbelt=false
```

## CLI Implementation (lot-cli/src/main.rs)

Follow reel-cli's pattern: thin wrapper, all sandbox logic in the library.

```rust
use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "lot", about = "Cross-platform process sandboxing")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a program inside a sandbox.
    Run(RunArgs),
    /// Configure platform prerequisites.
    Setup(SetupArgs),
    /// Show platform sandboxing capabilities.
    Probe,
}

#[derive(clap::Args)]
struct RunArgs {
    /// Path to YAML sandbox config file.
    #[arg(short, long)]
    config: PathBuf,
    /// Wall-clock timeout in seconds.
    #[arg(short, long)]
    timeout: Option<u64>,
    /// Print sandbox setup details to stderr.
    #[arg(short, long)]
    verbose: bool,
    /// Validate config and print effective policy without spawning.
    #[arg(long)]
    dry_run: bool,
    /// Program and arguments (after --).
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<OsString>,
}

#[derive(clap::Args)]
struct SetupArgs {
    /// Check prerequisites without modifying anything.
    #[arg(long)]
    check: bool,
    /// Print details of what is being configured.
    #[arg(long)]
    verbose: bool,
}
```

### Config deserialization

A `SandboxConfig` struct mirrors the YAML schema and deserializes via serde:

```rust
#[derive(serde::Deserialize, Default)]
struct SandboxConfig {
    #[serde(default)]
    filesystem: FilesystemConfig,
    #[serde(default)]
    network: NetworkConfig,
    #[serde(default)]
    limits: LimitsConfig,
    #[serde(default)]
    environment: EnvironmentConfig,
    #[serde(default)]
    process: ProcessConfig,
}

#[derive(serde::Deserialize, Default)]
struct FilesystemConfig {
    #[serde(default)]
    read: Vec<PathBuf>,
    #[serde(default)]
    write: Vec<PathBuf>,
    #[serde(default)]
    exec: Vec<PathBuf>,
    #[serde(default)]
    include_platform_exec: bool,
    #[serde(default)]
    include_platform_lib: bool,
    #[serde(default)]
    include_temp: bool,
}

#[derive(serde::Deserialize, Default)]
struct NetworkConfig {
    #[serde(default)]
    allow: bool,
}

#[derive(serde::Deserialize, Default)]
struct LimitsConfig {
    max_memory_bytes: Option<u64>,
    max_processes: Option<u32>,
    max_cpu_seconds: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct EnvironmentConfig {
    #[serde(default)]
    forward_common: bool,
    #[serde(default)]
    vars: std::collections::HashMap<String, String>,
}

#[derive(serde::Deserialize, Default)]
struct ProcessConfig {
    cwd: Option<PathBuf>,
}
```

### cmd_run flow

1. Read and parse YAML config file.
2. Build `SandboxPolicy` via `SandboxPolicyBuilder` using config fields.
3. Build `SandboxCommand` from trailing args, env config, and cwd.
4. If `--dry-run`: print effective policy to stderr, exit 0.
5. If `--verbose`: print policy summary and platform info to stderr.
6. Call `lot::spawn(&policy, command)`.
7. If `--timeout`: use `wait_with_output_timeout()` (requires tokio). On timeout, exit 124.
8. Otherwise: call `wait()`, forward child's exit code.

### Timeout handling

When `--timeout` is specified, the CLI needs async for `wait_with_output_timeout()`. Use `tokio::runtime::Builder::new_current_thread()` to create a runtime only when needed — no async overhead for the common no-timeout path.

Setup and probe remain synchronous — no change.

## Integration Tests

`tests/integration.rs` currently lives at the crate root and references `lot::` directly. After the restructure, it should stay in `lot/tests/integration.rs` (inside the library crate) so it continues to test the library.

## CI Changes

### Windows test job

Add `lot setup` before running tests:

```yaml
test-windows:
  name: Test (Windows)
  runs-on: windows-latest
  steps:
    - uses: actions/checkout@v4
    - run: rustup show
    - uses: actions/cache@v4
      with:
        path: |
          ~/.cargo/registry
          ~/.cargo/git
          target
        key: ${{ runner.os }}-cargo-test-${{ hashFiles('**/Cargo.lock') }}
        restore-keys: ${{ runner.os }}-cargo-test-
    - name: Setup AppContainer prerequisites
      run: cargo run -p lot-cli -- setup --verbose
    - run: cargo test -p lot -- --test-threads=1
```

### Other jobs

Update commands to target workspace or specific packages:

| Command | Before | After |
|---|---|---|
| Format | `cargo fmt --all --check` | `cargo fmt --all --check` (unchanged) |
| Clippy | `cargo clippy --all-targets -- -D warnings` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Build | `cargo build` | `cargo build --workspace` |
| Test (each platform) | `cargo test -- --test-threads=1` | `cargo test -p lot -- --test-threads=1` |

## Migration Steps

1. Create workspace root `Cargo.toml` with `[workspace]` config.
2. Create `lot/` subdirectory. Move `src/`, `tests/`, current `Cargo.toml` into it.
3. Adapt `lot/Cargo.toml` to reference `workspace = true` for shared fields.
4. Create `lot-cli/Cargo.toml` and `lot-cli/src/main.rs`.
5. Update `.github/workflows/ci.yml`.
6. Update `Cargo.lock` (regenerated by cargo).
7. Verify: `cargo build --workspace`, `cargo clippy --workspace --all-targets`, `cargo test -p lot`.

## Downstream Impact

reel-cli depends on lot via git rev. After this change, reel-cli's dependency becomes:

```toml
lot = { git = "https://github.com/bitmonk8/lot", rev = "...", package = "lot" }
```

No API changes — the library crate name stays `lot`.
