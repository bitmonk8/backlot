# Configuration

## Goal

Epic must work on any software project. All project-specific behavior is configured, not hardcoded.

## Configuration File

Project configuration lives in `epic.toml` at the project root (TOML format). User-level defaults in `~/.config/epic/config.toml`, overridden field-by-field by the project file. See [Configuration Resolution](#configuration-resolution) for full precedence.

### Verification Steps

```toml
[[verification]]
name = "Build"
command = ["cargo", "build"]
timeout = 300

[[verification]]
name = "Lint"
command = ["cargo", "clippy", "--", "-D", "warnings"]
timeout = 300

[[verification]]
name = "Test"
command = ["cargo", "test"]
timeout = 600
```

Example for a Python project:
```toml
[[verification]]
name = "Build"
command = ["python", "please.py", "build"]
timeout = 300

[[verification]]
name = "Lint"
command = ["python", "please.py", "lint"]
timeout = 300

[[verification]]
name = "Test"
command = ["python", "please.py", "test"]
timeout = 600
```

### Model Preferences

```toml
[models]
fast = "claude-haiku-4-5-20251001"   # Assessment, checkpoints, document ops
balanced = "claude-sonnet-4-6"      # Implementation, branch verification
strong = "claude-opus-4-6"          # Recovery, complex decomposition

# Or override with provider-specific model IDs
# fast = "custom:https://my-endpoint.com/haiku"
```

### Project Paths

```toml
[project]
root = "."                    # Project root (default: config file location)
epic_dir = ".epic"            # Epic working directory
```

### Agent Runtime

```toml
[agent]
runtime = "flick"             # Agent runtime (library crate dependency)
```

## Depth and Budget Configuration

```toml
[limits]
max_depth = 8                 # Task tree depth cap
max_recovery_rounds = 2       # Per branch
retry_budget = 3              # Retries per model tier per leaf
branch_fix_rounds = 3         # Verification fix rounds per branch
root_fix_rounds = 4           # Verification fix rounds for root task (extra Opus round)
max_total_tasks = 100         # Maximum total tasks (including root) allowed in a single run. Prevents unbounded cost growth.
```

## Init: `epic init`

Agent-driven interactive configuration scaffolding.

### Flow

1. **Explore** — An agent scans the project directory for build system markers and tooling:
   - Build systems: `Cargo.toml`, `package.json`, `pyproject.toml`, `Makefile`, `CMakeLists.txt`, `build.gradle`, `go.mod`, etc.
   - Test frameworks: presence of test directories, test config files (`jest.config`, `pytest.ini`, `.cargo/config.toml` test settings)
   - Linters/formatters: `clippy`, `eslint`, `ruff`, `black`, `prettier`, `golangci-lint`, etc.
   - CI config: `.github/workflows/`, `.gitlab-ci.yml` — extract existing build/test/lint commands as hints

2. **Present findings** — Report what was detected:
   - "Found Cargo.toml (Rust project). Detected: `cargo build`, `cargo test`, `cargo clippy`."
   - "Found `.github/workflows/ci.yml` with additional lint step: `cargo fmt --check`."

3. **Confirm interactively** — Ask the user which verification steps to enable:
   - Pre-filled with detected commands
   - User can accept, modify, add, or decline each step
   - Ask about model preferences (or accept defaults)
   - Ask about depth/budget limits (or accept defaults)

4. **Write `epic.toml`** — Generate config with confirmed choices. Declined options included as comments for reference. Result is always a valid, minimal config file.

### Fallback

If no project markers are detected, scaffold a minimal `epic.toml` with empty `[[verification]]` sections and commented examples. Warn the user that verification steps need manual configuration.

## CLI Interface

### Subcommands

```
epic init                      # Agent-driven interactive config scaffolding
epic run <goal>                # Start a new run with the given goal
epic resume                   # Resume a previously interrupted run from .epic/state.json
epic status                   # Show the current status of a run
```

### Global Options

| Flag                | Env var                | Default        | Description                                      |
|---------------------|------------------------|----------------|--------------------------------------------------|
| `--credential`      | `EPIC_CREDENTIAL`      | `anthropic`    | Credential name passed to Flick                  |
| `--no-tui`          | `EPIC_NO_TUI`          | off            | Disable the TUI; run headless with event output to stderr |
| `--no-sandbox-warn` | `EPIC_NO_SANDBOX_WARN` | off            | Suppress the warning when no container/VM is detected |

Global options go before the subcommand:

```
epic --no-tui run "fix the login bug"
epic --credential my-key resume
```

## Configuration Resolution

Project config (highest priority wins):
1. `epic.toml` in current directory
2. `.epic/config.toml` in current directory
3. Defaults (no verification steps — warn and proceed)

User-level defaults (`~/.config/epic/config.toml`) are loaded first and overridden field-by-field by the project config above.

## Open Questions

- ~~Config format: TOML vs YAML vs RON?~~ — **Decided: TOML.**
- ~~Should `epic init` generate a starter config file?~~ — **Decided: Yes, agent-driven interactive init.** See Init section below.
- ~~How much agent runtime config belongs in epic's config?~~ — **Decided: Epic owns all config.** The `[agent]` section in `epic.toml` exposes Flick-specific knobs.
