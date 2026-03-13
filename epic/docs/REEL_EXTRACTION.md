# Reel Extraction

Status: **complete**.

**Repository**: https://github.com/bitmonk8/reel — workspace with `reel` library + `reel-cli` binary crates. Local path: `C:\UnitySrc\reel`.

## Architecture

Three layers of AI agent abstraction:

| Layer | Project | Scope |
|---|---|---|
| Conversation turn | Flick | One model call, one reply. No tools, no side effects. |
| Agent session | Reel | One request with tools configured. Tool loop runs until the model returns a final response. Side effects via tools. |
| Orchestration | Epic | Multi-task tree. Retry, escalation, recovery, state persistence, TUI. |

Reel owns: "give an agent a request and tools, get back a structured response after it uses those tools."

Epic owns: "decompose a goal into tasks, execute them in order, verify, retry, recover."

```
Epic (orchestrator)
  │
  │ AgentService trait (9 epic-specific methods)
  │ TaskContext, prompts, wire types
  │
  ▼
ReelAgent (thin adapter)
  │  Builds AgentRequestConfig per phase, delegates to reel
  │
  ▼
reel::Agent::run(&AgentRequestConfig, query) → RunResult<T>
  │
  │  Reel boundary
  │
  ▼
Reel (agent session)
  ├── Tool loop (Flick client.run → tool dispatch → client.resume → repeat)
  ├── 6 built-in tools (Read/Write/Edit/Glob/Grep/NuShell)
  ├── ToolHandler trait for custom tool dispatch
  ├── NuSession (nu --mcp + lot sandbox)
  └── ToolGrant bitflags (WRITE/NU)
  │
  ▼
Flick (conversation turn)
Lot (process sandbox)
```

## Public API

```rust
pub struct Agent { /* ... */ }

impl Agent {
    pub fn new(env: AgentEnvironment) -> Self;
    pub async fn run<T: DeserializeOwned>(&self, request: &AgentRequestConfig, query: &str) -> Result<RunResult<T>>;
    pub fn build_effective_config(request: &AgentRequestConfig) -> Result<RequestConfig>;
}

pub struct AgentEnvironment {
    pub model_registry: reel::ModelRegistry,
    pub provider_registry: reel::ProviderRegistry,
    pub project_root: PathBuf,
    pub timeout: Duration,
}

pub struct AgentRequestConfig {
    pub config: reel::RequestConfig,
    pub grant: ToolGrant,
    pub custom_tools: Vec<Box<dyn ToolHandler>>,
}

pub struct RunResult<T> {
    pub output: T,
    pub usage: Option<Usage>,
    pub tool_calls: u32,
    pub response_hash: Option<String>,
}
```

## What lives where

### In reel

| Module | Content |
|---|---|
| `agent.rs` | `Agent` runtime, tool loop, `AgentEnvironment`, `AgentRequestConfig`, `RunResult`, `Usage` |
| `tools.rs` | Tool definitions, execution, translation layer, `ToolGrant`, `ToolDefinition`, `ToolExecResult` |
| `nu_session.rs` | NuShell MCP client, sandbox policy, process lifecycle |
| `build.rs` | Nu binary download, config file generation |

### In epic

| Module | Content |
|---|---|
| `agent/mod.rs` | `AgentService` trait, `TaskContext`, sibling/child summaries |
| `agent/prompts.rs` | Prompt builders (all reference `TaskContext`) |
| `agent/wire.rs` | Wire format types, output schemas |
| `agent/reel_adapter.rs` | `ReelAgent` — thin adapter: builds `AgentRequestConfig`, delegates to `reel::Agent`, converts results |

## Custom tool dispatch

Reel defines `ToolHandler` trait. Consumers implement it for domain-specific tools (e.g., Research Service). Custom tools dispatch before built-ins, allowing override. Custom tools are not governed by `ToolGrant` flags.

## CLI

See `C:\UnitySrc\reel\docs\CLI_TOOL.md`. Commands: `reel run` (agent session), `reel setup` (platform prerequisites). Config is a YAML superset of flick's `RequestConfig` with a `grant` field.
