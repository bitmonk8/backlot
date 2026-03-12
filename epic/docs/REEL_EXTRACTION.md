# Reel Extraction Spec

Status: **exploration** — identifying seams and challenges. No implementation.

## Concept

Three layers of AI agent abstraction:

| Layer | Project | Scope |
|---|---|---|
| Conversation turn | Flick | One model call, one reply. No tools, no side effects. |
| Agent session | Reel | One request with tools configured. Tool loop runs until the model returns a final response. Side effects via tools. |
| Orchestration | Epic | Multi-task tree. Retry, escalation, recovery, state persistence, TUI. |

Reel owns: "give an agent a request and tools, get back a structured response after it uses those tools."

Epic owns: "decompose a goal into tasks, execute them in order, verify, retry, recover."

## Current Architecture

Epic's `src/agent/` directory contains two distinct layers mixed together:

### Layer 1: Generic agent session (reel candidate)

These components implement "run a request with tools until done":

- **`flick.rs` — `run_with_tools()`**: The tool loop. Calls `client.run()`, dispatches tool calls, calls `client.resume()`, repeats until `Complete`. ~50 lines of core logic.
- **`flick.rs` — `run_structured()`**: Single call without tools. Wraps `client.run()` with timeout and structured output parsing.
- **`tools.rs` — tool definitions and execution**: `tool_definitions()` builds Flick tool schemas from a `ToolGrant`. `execute_tool()` dispatches JSON tool calls to nu commands. Translation layer (`translate_tool_call`, `format_tool_result`).
- **`nu_session.rs`**: NuShell MCP client. Spawns `nu --mcp` inside a lot sandbox, evaluates commands, manages lifecycle.
- **`tools.rs` — `ToolGrant` bitflags**: WRITE, NU permission flags controlling which tools are offered.

### Layer 2: Epic-specific agent logic (stays in epic)

These components encode epic's domain — task types, prompt templates, wire formats:

- **`prompts.rs`**: 8 prompt builders (`build_assess`, `build_execute_leaf`, `build_verify`, etc.) that format `TaskContext` into system/user prompts. Every prompt references epic concepts: task tree position, sibling context, verification criteria, checkpoint guidance, recovery strategy.
- **`config_gen.rs`**: Wire format types (`AssessmentWire`, `CheckpointWire`, `DecompositionWire`, etc.) and Flick `RequestConfig` builders (`build_assess_config`, `build_execute_leaf_config`, etc.). Each config builder sets model key, output schema, and tool list for a specific epic agent method.
- **`mod.rs` — `AgentService` trait**: 9 async methods mapping 1:1 to epic's orchestrator needs (assess, execute_leaf, verify, checkpoint, fix_leaf, design_fix_subtasks, assess_recovery, design_recovery_subtasks, design_and_decompose).
- **`mod.rs` — `TaskContext`**: Context bundle with parent goals, sibling summaries, checkpoint guidance, child statuses — all epic orchestration concepts.
- **`flick.rs` — `FlickAgent` impl of `AgentService`**: 9 method implementations that each: (1) call a `prompts::build_*` function, (2) call a `config_gen::build_*_config` function, (3) delegate to `run_structured` or `run_with_tools`, (4) convert wire types to domain types.

## The Seam

The natural extraction boundary is between `run_with_tools`/`run_structured` and the 9 `AgentService` methods that call them.

```
Epic (orchestrator)
  │
  │ AgentService trait (9 epic-specific methods)
  │ TaskContext, prompts, wire types, config builders
  │
  ▼
FlickAgent.run_with_tools(config, query, grant) → T
FlickAgent.run_structured(config, query) → T
  │
  │  ← This is the reel boundary
  │
  ▼
Reel (agent session)
  ├── Tool loop (Flick client.run → tool dispatch → client.resume → repeat)
  ├── Tool definitions + execution (translate_tool_call, format_tool_result)
  ├── NuSession (nu --mcp + lot sandbox)
  └── ToolGrant bitflags
  │
  ▼
Flick (conversation turn)
Lot (process sandbox)
```

## Design

Reel owns: Agent struct, tool loop, tool definitions, NuSession, ToolGrant, and a tool extension mechanism. Reel provides the 6 built-in tools (Read/Write/Edit/Glob/Grep/NuShell) as defaults.

Epic (or any consumer) adds domain-specific tools (e.g., "research_query") by implementing a reel trait.

```rust
pub struct Agent { /* ... */ }

impl Agent {
    pub fn new(env: AgentEnvironment) -> Self;
    pub async fn run<T: DeserializeOwned>(&self, request: AgentRequest) -> Result<RunResult<T>>;
}

pub struct AgentEnvironment {
    pub flick_config: flick::Config,  // Built once with models + providers defined
    pub project_root: PathBuf,
    pub timeout: Duration,
}

pub struct AgentRequest {
    pub system_prompt: String,
    pub query: String,
    pub model: String,                      // Named model key (e.g., "fast", "balanced", "strong")
    pub grant: ToolGrant,
    pub output_schema: Option<Value>,       // JSON Schema for structured output
    pub custom_tools: Vec<Box<dyn ToolHandler>>,  // Consumer-provided tools
}

pub struct RunResult<T> {
    pub output: T,
    pub usage: Usage,
    pub tool_calls: u32,
}
```

Epic doesn't touch Flick directly — it talks to reel. Reel selects models and builds `flick::RequestConfig` internally from `AgentRequest`.

**Depends on**: Flick named models spec (in flick repo: `docs/NAMED_MODELS.md`) — Flick must support named models and per-call override methods before reel can use this API cleanly.

### AgentEnvironment vs AgentRequest

`AgentEnvironment` holds the shared runtime context that doesn't change between calls: the base Flick config (with named models and providers defined), project root, and call timeout. Created once, shared across calls.

`AgentRequest` holds per-call parameters: prompt, model name, grant, output schema, custom tools. A new `AgentRequest` is built for every agent call. The `model` field references a named model from the Flick config's `models` map (e.g., `"fast"`, `"balanced"`, `"strong"`).

Custom tools live on `AgentRequest` (not `AgentConfig`) because different agent phases need different tools. An execute-leaf call might get the Research Service tool; an assessment call gets no custom tools. The consumer decides per-call.

### What moves to reel

| Current file | Lines | Content |
|---|---|---|
| `agent/flick.rs` (partial) | ~100 | `run_with_tools`, `run_structured`, `build_client`, helpers |
| `agent/tools.rs` | ~1300 | Tool definitions, execution, translation layer, `ToolGrant` |
| `agent/nu_session.rs` | ~1750 | NuShell MCP client, sandbox policy, process lifecycle |
| `build.rs` | — | Nu binary download, config file generation |

### What stays in epic

| Current file | Content |
|---|---|
| `agent/mod.rs` | `AgentService` trait, `TaskContext`, sibling/child summaries |
| `agent/prompts.rs` | Prompt builders (all reference `TaskContext`) |
| `agent/config_gen.rs` | Wire format types, Flick config builders |
| `agent/flick.rs` (partial) | `FlickAgent` struct, `AgentService` impl (becomes thin adapter over `reel::Agent`) |

### Extraction sequence

1. Define reel's public API (`Agent`, `AgentConfig`, `AgentRequest`, `RunResult`, `ToolGrant`)
2. Move tool loop, tool definitions, tool execution, NuSession, build.rs nu management into reel
3. Epic's `FlickAgent` becomes a thin adapter: builds `AgentRequest` from `TaskContext` + prompts, calls `reel::Agent::run()`, converts the result
4. Epic's `AgentService` trait, `TaskContext`, prompts, wire types, config builders stay in epic
5. Epic depends on reel; reel depends on flick + lot

## Challenges

### 1. Type ownership — where do shared types live?

Types currently used by both layers:

| Type | Used by | Current location |
|---|---|---|
| `ToolGrant` (bitflags) | Tool definitions, nu session, tool execution | `tools.rs` |
| `Model` (Haiku/Sonnet/Opus) | Config builders, agent service | `task/mod.rs` |
| `ModelConfig` | Flick config construction | `config/project.rs` |
| `VerificationStep` | Prompt assembly | `config/project.rs` |

`ToolGrant` belongs in reel. `Model`, `ModelConfig`, and `VerificationStep` stay in epic.

Reel takes `model: &str` (the actual API model ID). Epic owns the tier abstraction and the mapping. Keeps reel provider-agnostic.

### 2. build.rs and nu binary management

Epic's `build.rs` downloads and caches the nu binary in `target/nu-cache/`. Config files (`epic_config.nu`, `epic_env.nu`) are also written there.

If reel owns NuSession, it needs the nu binary and config files. Recommended approach: reel ships nu config as embedded resources (`include_str!`) and resolves the binary at runtime (same-dir, cache, PATH). `build.rs` for binary download lives in reel. Epic's build.rs becomes minimal or empty.

### 3. Prompt assembly

`prompts.rs` is deeply epic-specific (references `TaskContext`, sibling summaries, checkpoint guidance). It stays in epic. Reel takes strings — epic builds them.

### 4. The `ClientFactory` / `ToolExecutor` test seams

`FlickAgent` uses injected `ClientFactory` and `ToolExecutor` traits for testing. These seams move to reel's `Agent` struct.

### 5. Event emission

Currently, `run_with_tools` calls `log_usage()` which writes to stderr. No event channel exists inside the agent layer.

For v1: reel returns metadata alongside the result (`RunResult<T>` with usage and tool call count). No streaming events. Streaming can be added later via an optional callback.

### 6. Custom tool extension

Epic's Research Service is planned but not yet implemented (see STATUS.md). When implemented, it will need to be a custom tool beyond the 6 built-ins.

**Current tool architecture**: All 6 built-in tools execute through the sandboxed nu process via MCP. There are no in-process tool callbacks today. The flow is: agent requests tool call (JSON) → `execute_tool()` dispatches by name → `translate_tool_call()` converts to nu command → `NuSession::evaluate()` runs it in the sandbox → `format_tool_result()` formats the response.

**Why Research Service can't be a nu script**: The Research Service isn't just file I/O — it's an agent-within-an-agent. It checks a document store for existing knowledge, identifies gaps, then spawns a Haiku agent call to fill those gaps via codebase exploration or web search. A nu script cannot make API calls through Flick/reel.

**Decided approach: tool handler trait.** See [Custom Tool Dispatch](#custom-tool-dispatch) below.

## Decisions

- **CLI + library dual nature**: Reel follows Flick's approach — both a CLI tool (for testing and experimentation) and a library (for embedding by epic and other consumers). CLI interface and config format TBD.
- **Nu binary provisioning**: Build-time download, same as current epic approach. NuShell changes frequently; reel must pin a known-good version. The shell exists for reel's tool execution, not for end-user interaction.
- **Extraction is worth it**: ~3000 lines move to reel, epic shrinks from ~6000+ to ~2700 agent/orchestrator lines. Epic becomes focused on orchestration. Reel provides standalone value as a general-purpose agent-session tool — usable without epic.
- **Custom tool dispatch**: Tool handler trait. See below.

---

## Custom Tool Dispatch

### Problem

Reel's 6 built-in tools all route through the sandboxed nu process. But consumers need to add domain-specific tools that run in-process (e.g., Research Service spawns nested agent calls, accesses a document store). These cannot route through nu.

### Design

Reel defines a `ToolHandler` trait. Consumers implement it for each custom tool, bundling the tool definition (what the model sees) with the execution logic (what happens when called).

```rust
/// A tool definition as seen by the model.
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema
}

/// Result of executing a tool call.
pub struct ToolExecResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Consumer-implemented trait for custom tools.
///
/// Each implementation bundles the tool's schema (what the model sees)
/// with its execution logic (what happens when the model calls it).
/// The implementing struct captures whatever state it needs — database
/// handles, agent references, config — as fields.
pub trait ToolHandler: Send + Sync {
    /// Returns the tool definition included in the model's tool list.
    fn definition(&self) -> ToolDefinition;

    /// Executes the tool call. Called by reel's tool loop when the model
    /// invokes a tool whose name matches `definition().name`.
    async fn execute(&self, tool_use_id: String, input: &serde_json::Value) -> ToolExecResult;
}
```

### Dispatch order

Reel's tool loop dispatches each tool call as follows:

1. **Custom tools first**: Check `request.custom_tools` for a handler whose `definition().name` matches the tool name. If found, call `handler.execute()`.
2. **Built-in tools**: If no custom handler matches, route to the built-in tool executor (nu session).
3. **Unknown tool**: Return an error result to the model.

Custom tools take priority so consumers can override built-in tool behavior if needed (unlikely but possible).

### Tool list assembly

Reel assembles the model's tool list from two sources:
- Built-in tools filtered by `ToolGrant` (from `tool_definitions(grant)`)
- Custom tool definitions (from `request.custom_tools.iter().map(|h| h.definition())`)

Both are merged into the Flick config's tool list before the first model call.

### Consumer example

```rust
// In epic — Research Service as a custom tool
struct ResearchTool {
    agent: Arc<reel::Agent>,
    doc_store: Arc<DocumentStore>,
}

impl reel::ToolHandler for ResearchTool {
    fn definition(&self) -> reel::ToolDefinition {
        reel::ToolDefinition {
            name: "research_query".into(),
            description: "Query the knowledge base...".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "scope": { "type": "string", "enum": ["PROJECT", "WEB", "BOTH"] }
                },
                "required": ["question"]
            }),
        }
    }

    async fn execute(&self, tool_use_id: String, input: &serde_json::Value) -> reel::ToolExecResult {
        let question = input["question"].as_str().unwrap_or("");
        // Can use self.agent to spawn a nested reel session
        // Can use self.doc_store to check/update documents
        let answer = self.doc_store.query(question).await;
        reel::ToolExecResult {
            tool_use_id,
            content: answer,
            is_error: false,
        }
    }
}
```

### Grant interaction

Custom tools are not governed by `ToolGrant` flags. Grant flags control the built-in tool set and the nu sandbox policy. Custom tools execute in the consumer's process — the consumer is responsible for any access control on custom tool behavior.

### No open questions remaining

The custom tool dispatch design is decided. Implementation details (trait object boxing, lifetime management) will be resolved during extraction.

