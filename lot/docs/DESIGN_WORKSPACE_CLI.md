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
lot = { path = "../lot" }
clap = { version = "4", features = ["derive"] }
```

No tokio dependency — setup and probe are synchronous.

## CLI Commands

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

Follow reel-cli's pattern:

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
    /// Configure platform prerequisites.
    Setup(SetupArgs),
    /// Show platform sandboxing capabilities.
    Probe,
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

Setup implementation: same pattern as reel-cli `cmd_setup` — `#[cfg(target_os = "windows")]` / `#[cfg(not(...))]` function pairs.

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
