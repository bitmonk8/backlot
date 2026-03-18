# reel

Agent session runtime for Rust — tool loop, NuShell sandbox, built-in tool execution.

Reel sits between [flick](../flick) (single-shot LLM call) and [epic](../epic) (task orchestrator). It owns the tool loop: up to 50 rounds / 200 total tool calls of tool-call dispatch, a sandboxed NuShell MCP session via [lot](../lot), and 6 built-in tools. Available as both a Rust library (`reel`) and a thin CLI binary (`reel-cli`, binary name `reel`).

## Workspace

| Crate | Type | Description |
|-------|------|-------------|
| `reel` | library | Agent runtime — tool loop, NuShell session, built-in tools, sandbox re-exports |
| `reel-cli` | binary | CLI interface wrapping the library |

## Relationship to siblings

| Project | Role |
|---------|------|
| lot | Process sandboxing — AppContainer (Windows), namespaces + seccomp (Linux), Seatbelt (macOS) |
| flick | LLM primitive — single model call, tool declaration (not execution), JSON result |
| reel | Agent session — tool loop, NuShell sandbox, built-in tool execution |
| epic | Orchestrator — recursive task decomposition, verification, recovery, TUI |

## Design principles

- **Dual interface.** Library crate + thin CLI binary. All logic in the library.
- **Tool-loop agent.** Request-dispatch-response cycles up to 50 rounds / 200 total tool calls. No streaming.
- **Sandboxed execution.** All tools run through a NuShell MCP session sandboxed by lot. No unsandboxed fallback.
- **Grant-based access control.** Bitflags (TOOLS, WRITE, NETWORK) determine tool availability and sandbox policy. WRITE and NETWORK imply TOOLS. Network denied by default.
- **Eager NuShell spawn.** Process started at session creation (if TOOLS granted), not on first use.
- **Separation of concerns.** Reel handles tool execution. Flick handles LLM calls. Lot handles OS-level sandboxing.

## Requirements

Rust 1.85+ (edition 2024).

## Build

```sh
cargo build --release
```

The build script downloads prebuilt NuShell 0.111.0 and ripgrep 14.1.1 binaries for the target platform, verifies SHA-256 checksums, and caches in `target/nu-cache/`. Set `NU_SKIP_DOWNLOAD=1` or `RG_SKIP_DOWNLOAD=1` for offline builds.

## Quick start

1. Register a provider and model via flick:

```sh
flick provider add anthropic
flick model add fast
```

2. Create a reel config file (`agent.yaml`):

```yaml
model: fast
system_prompt: "You are a code assistant."
grant:
  - tools
  - write
```

3. Run a query:

```sh
reel run --config agent.yaml --query "list all Rust files"
```

Or pipe from stdin:

```sh
echo "explain this code" | reel run --config agent.yaml
```

## Library usage

```rust
use reel::{Agent, AgentEnvironment, AgentRequestConfig, ToolGrant, RequestConfig, ConfigFormat, ModelRegistry, ProviderRegistry};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let providers = ProviderRegistry::load_default()?;
    let models = ModelRegistry::load_default().await?;

    let env = AgentEnvironment {
        model_registry: models,
        provider_registry: providers,
        project_root: std::env::current_dir()?,
        timeout: Duration::from_secs(120),
    };

    let agent = Agent::new(env);

    let config = RequestConfig::from_str(
        &std::fs::read_to_string("agent.yaml")?,
        ConfigFormat::Yaml,
    )?;

    let request = AgentRequestConfig {
        config,
        grant: ToolGrant::TOOLS | ToolGrant::WRITE,
        custom_tools: Vec::new(),
    };

    let result: reel::RunResult<String> = agent.run(&request, "list all Rust files").await?;
    println!("{}", result.output);
    Ok(())
}
```

The config parsing above does not strip the `grant` key from YAML. Library consumers build `AgentRequestConfig` directly — the YAML stripping is a CLI convenience. For library use, parse flick's `RequestConfig` from YAML that does NOT contain the `grant` key, then set `grant` programmatically.

### Custom tools via `ToolHandler`

```rust
pub trait ToolHandler: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn execute<'a>(
        &'a self,
        tool_use_id: String,
        input: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolExecResult> + Send + 'a>>;
}
```

Custom tools are added to `AgentRequestConfig.custom_tools`. They are dispatched alongside built-in tools in the tool loop.

## CLI reference

```
reel run --config <PATH> [OPTIONS]
reel setup [OPTIONS]
```

### `reel run`

| Flag | Description |
|------|-------------|
| `--config <path>` | Path to reel config file (YAML) (required) |
| `--query <text>` | Query text; reads from stdin if omitted |
| `--project-root <path>` | Working directory for tool execution (default: cwd) |
| `--timeout <seconds>` | Agent timeout in seconds — applied per model call (default: 120, minimum: 1) |
| `--dry-run` | Build the request and print it without calling the model (includes resolved grant names) |

When `--query` is omitted and stdin is a TTY, prints `Reading query from stdin (Ctrl+D to submit)...` to stderr.

### `reel setup`

Platform prerequisite configuration. Windows-only (no-op on other platforms).

| Flag | Description |
|------|-------------|
| `--check` | Check prerequisites without modifying anything (exit 0 if OK, 1 if not) |
| `--verbose` | Print details of what is being configured |

Grants AppContainer ACLs (NUL device access + ancestor directory traverse ACEs). Requires elevation on Windows.

## Output format

Success (`reel run`):

```json
{
  "status": "Ok",
  "content": "The agent's final text response",
  "usage": { "input_tokens": 1234, "output_tokens": 567, "cost_usd": 0.02 },  // null if unavailable
  "tool_calls": 12,
  "response_hash": "abc123"  // null if unavailable
}
```

`content` is a string for free-form output. When `output_schema` is configured, `content` is the schema-shaped JSON object.

Error:

```json
{
  "status": "Error",
  "error": { "code": "cli_error", "message": "Model 'gpt-5' not found in registry" }
}
```

Errors go to stdout as JSON with exit code 1. Human-readable messages go to stderr.

## Configuration

Full example:

```yaml
# Flick RequestConfig fields (passed through)
model: balanced
system_prompt: "You are a code assistant."
temperature: 0.0
reasoning:
  level: medium

# Reel-specific fields
grant:
  - tools
  - write
  - network
```

| Field | Type | Description |
|-------|------|-------------|
| `model` | string | Key into flick's ModelRegistry (`~/.flick/models`) |
| `system_prompt` | string | System prompt sent to the model |
| `temperature` | float | Sampling temperature (optional) |
| `reasoning` | object | Reasoning budget: `level` = minimal/low/medium/high (optional) |
| `output_schema` | object | JSON Schema for structured model output (optional) |
| `grant` | list | Tool grants: `tools`, `write`, `network`. `write` and `network` imply `tools`. Omit for structured mode (no tools) |

Reel config is a superset of flick's `RequestConfig`. The CLI strips the `grant` key before passing to flick (which uses `deny_unknown_fields`). Library consumers build `AgentRequestConfig` directly.

## Built-in tools

| Tool | Grant Required | Description |
|------|----------------|-------------|
| Read | TOOLS | Read file contents |
| Write | WRITE | Create or overwrite a file |
| Edit | WRITE | Replace exact substring in a file |
| Glob | TOOLS | Find files by glob pattern (max 1000 results) |
| Grep | TOOLS | Search file contents by regex (max 64 KiB output) |
| NuShell | TOOLS | Execute arbitrary NuShell command (timeout: 120s default, 600s max) |

All tools execute through the NuShell MCP session as custom commands (`reel read`, `reel write`, etc.) or direct evaluation (NuShell tool). All tool output is truncated to 64 KiB.

## Grant flags

| Flag | Effect |
|------|--------|
| `TOOLS` | Enables tool loop and read-only tools (Read, Glob, Grep, NuShell). Without TOOLS, agent runs in structured-output mode (no tools). |
| `WRITE` | Enables Write and Edit tools. Grants sandbox write access to project root. Implies `TOOLS`. |
| `NETWORK` | Enables outbound network from sandbox. Denied by default. Implies `TOOLS`. |

## Sandboxing

Reel sandboxes the NuShell process via lot:

- **Windows**: AppContainer + Job Objects
- **Linux**: User/mount/PID/net namespaces + seccomp-BPF + cgroups v2
- **macOS**: Seatbelt profiles + setrlimit

The sandbox policy is derived from the grant flags:

- Read-only tools: project root is read-only in sandbox
- With WRITE: project root is read-write in sandbox
- With NETWORK: outbound network allowed
- Without NETWORK: network stack isolated (denied by default)

On Windows, `reel setup` must be run once (as administrator) before sandboxed execution works. This grants NUL device access and ancestor directory traverse ACEs for AppContainer.

## Testing

```sh
cargo test
```

206 tests (191 reel + 15 reel-cli). Integration tests require NuShell binary (downloaded by build.rs).

## Dependencies

| Crate | Purpose |
|-------|---------|
| `flick` | LLM client (model calling, config parsing, provider abstraction) |
| `lot` | Process sandboxing (AppContainer, namespaces, Seatbelt) |
| `tokio` | Async runtime |
| `anyhow` | Error handling |
| `serde` + `serde_json` | Serialization |
| `bitflags` | Tool grant flags |
| `tempfile` | Per-session temp directories |

Build dependencies: `ureq` (HTTP), `sha2` (checksums), `flate2` + `tar` + `zip` (archive extraction).

## License

MIT
