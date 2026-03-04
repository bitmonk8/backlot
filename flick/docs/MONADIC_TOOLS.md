# Flick — Monadic Tool Architecture

## Summary

Remove all tool execution from Flick. Flick declares tool definitions to the model but never executes tools. When the model makes tool calls, Flick yields control back to the caller with the updated context. The caller executes tools, appends results, and re-invokes Flick with the previous context hash plus tool results. This continues until the model produces a final response with no tool calls.

Additionally: Flick's output changes from streaming JSON-lines events to a single JSON result object. One invocation = one JSON object on stdout.

## Motivation

1. **Sandboxing complexity.** The current sandbox system (wrapper prefix, policy generation, platform-specific logic) adds significant complexity for limited value. Moving tool execution out of Flick eliminates this entirely.
2. **Builtin tools are generic.** `read_file`, `write_file`, `list_directory`, `shell_exec` are not LLM-specific. The caller (Epic) already has better implementations with richer policy controls.
3. **Custom tool plumbing.** Template expansion, executable piping, command injection guards, timeout management — all removed from Flick.
4. **Separation of concerns.** Flick becomes a pure LLM interface: config in, model call, events out. Tool execution is the caller's responsibility.
5. **Composability.** The caller can implement any tool execution strategy (sandboxed, remote, mocked, cached) without Flick needing to know.

## Current Architecture (Before)

```
Caller → flick run --config f.toml --query "do X"
         │
         ├─ Flick builds request (tools from config + builtins)
         ├─ Flick calls LLM
         ├─ Model returns tool_calls
         ├─ Flick executes tools internally (builtins + custom)
         ├─ Flick appends results to context
         ├─ Flick calls LLM again
         ├─ ... (loop until no tool calls)
         └─ Flick emits done event
```

Flick owns the full agent loop, tool execution, sandboxing, resource access control, and result collection.

## Proposed Architecture (After)

```
Caller → flick run --config f.toml --query "do X"
         ├─ Flick calls LLM
         ├─ Model returns tool_calls
         ├─ Flick writes context to ~/.flick/contexts/{hash}.json
         ├─ Flick prints JSON result: {status: "tool_calls_pending", content: [...], context_hash: "abc123"}
         └─ Exit

Caller executes tools externally

Caller → flick run --config f.toml --resume abc123 --tool-results results.json
         ├─ Flick loads context from ~/.flick/contexts/abc123.json
         ├─ Flick appends tool results to context
         ├─ Flick calls LLM
         ├─ Model returns text (no tool calls)
         ├─ Flick writes new context to ~/.flick/contexts/{hash2}.json
         ├─ Flick prints JSON result: {status: "complete", content: [...], context_hash: "def456"}
         └─ Exit
```

Flick calls the model exactly once per invocation. The caller drives the loop.

## Key Design Decisions

### Single-shot invocation

Each `flick run` makes exactly one LLM call and returns. No internal loop. The current 25-iteration agent loop is removed.

### Tool definitions remain in config

The config still declares tool schemas so Flick can include them in the model request. But config no longer specifies *how* to execute tools — no `command`, no `executable`, no sandbox config.

### Immutable context files

Every invocation writes a new context file to `~/.flick/contexts/{hash}.json`. Context files are never mutated. Each invocation produces a new hash. This is already the behavior today — it remains unchanged.

### Resume via context hash

The current `--context <file>` flag is replaced by `--resume <hash>`. Flick looks up `~/.flick/contexts/{hash}.json` to load the prior context. This is paired with `--tool-results <file>` to supply the tool results the caller executed.

### Tool results input format

The `--tool-results` flag accepts a JSON file containing an array of tool results:

```json
[
  {"tool_use_id": "tc_1", "content": "file contents here", "is_error": false},
  {"tool_use_id": "tc_2", "content": "command not found", "is_error": true}
]
```

This matches the existing `ToolResult` content block schema in `context.rs`.

### Status field signals completion

The result object's `status` field tells the caller what to do next:

- `"tool_calls_pending"` — extract tool calls from `content`, execute them, resume with `--resume` + `--tool-results`
- `"complete"` — session finished, no further action needed
- `"error"` — invocation failed, see `error` field for details

### Single JSON result (no streaming)

Flick no longer streams events. One invocation produces one JSON object on stdout. The `--raw` flag and the entire event/emitter system are removed. The result object contains all content blocks, tool calls, usage, and status in a single structure.

### Clean break

No backward compatibility or migration paths. This is a breaking change.

## CLI Changes

### Before

```
flick run --config <toml> [--query <text>] [--context <json>] [--raw] [--dry-run] [--model <id>] [--reasoning <level>]
```

### After

```
flick run --config <toml> [--query <text>] [--resume <hash>] [--tool-results <json>] [--dry-run] [--model <id>] [--reasoning <level>]
```

| Flag | Change |
|------|--------|
| `--context <json>` | **Replaced** by `--resume <hash>` |
| `--tool-results <json>` | **New** — tool results for resumed session |
| `--raw` | **Removed** — always JSON output |

Validation:
- `--resume` requires `--tool-results` (can't resume without providing results)
- `--tool-results` requires `--resume` (results need a session to attach to)
- `--query` and `--resume` are mutually exclusive (new session vs. continuation)

## What Gets Removed

| Component | Current | After |
|-----------|---------|-------|
| `src/tool.rs` | 1,320 lines — builtin tools, custom tool execution, resource access control, command runner | **Deleted entirely** |
| `src/sandbox.rs` | 320 lines — wrapper prefix, policy generation, placeholder expansion | **Deleted entirely** |
| `src/agent.rs` agent loop | 25-iteration loop with tool execution | **Single model call, return** |
| `src/config.rs` sandbox config | `SandboxConfig`, validation, resource access levels | **Deleted** |
| `src/config.rs` tool execution config | `command`, `executable` fields on custom tools | **Deleted** |
| `src/config.rs` builtin tool toggles | `read_file`, `write_file`, `list_directory`, `shell_exec` booleans | **Deleted** |
| `src/config.rs` resource config | `ResourceConfig`, path canonicalization, access levels | **Deleted** |
| `src/event.rs` | Event types, `JsonLinesEmitter`, `RawEmitter`, `RunSummary` | **Deleted entirely** — replaced by single result struct |
| `--raw` flag | Plain-text output | **Deleted** |

## What Changes

### Config: `[tools]` section

**Before:**
```toml
[tools]
read_file = true
write_file = true
shell_exec = true

[[tools.custom]]
name = "grep_project"
description = "Search for a pattern"
parameters = { type = "object", properties = { pattern = { type = "string" } }, required = ["pattern"] }
command = "rg {{pattern}} src/"
```

**After:**
```toml
[[tools]]
name = "read_file"
description = "Read a file's contents"
parameters = { type = "object", properties = { path = { type = "string" } }, required = ["path"] }

[[tools]]
name = "grep_project"
description = "Search for a pattern"
parameters = { type = "object", properties = { pattern = { type = "string" } }, required = ["pattern"] }
```

All tools are uniform: a name, a description, and a JSON schema. No distinction between builtin and custom. No execution config.

### Agent module

**Before:** Loop calling model → execute tools → call model → ...
**After:** Single model call. Build params, call provider, emit events, write context, return.

The module may be renamed or inlined since it no longer manages a loop.

### Output format

The entire event/emitter system (`event.rs`) is replaced by a single result struct serialized as one JSON object to stdout. No streaming, no JSON-lines, no emitters.

### `flick init`

**Removed prompts:** builtin tool toggles, sandbox configuration.
**Remaining prompts:** provider selection, model selection, max output tokens.
**Output:** generated config includes a commented-out `[[tools]]` template showing the schema for tool registration.

Example output:
```toml
[model]
id = "claude-sonnet-4-20250514"
provider = "anthropic"
max_output_tokens = 16384

# [[tools]]
# name = "tool_name"
# description = "What this tool does"
# parameters = { type = "object", properties = { arg = { type = "string" } }, required = ["arg"] }
```

## What Stays The Same

- CLI subcommands (`flick run`, `flick setup`, `flick init`, `flick list`)
- Provider system (Messages API, Chat Completions)
- Context format (`context.rs` types — `ToolUse`, `ToolResult` content blocks)
- Credential store
- Model registry
- History logging (`~/.flick/history.jsonl`)
- Context storage (`~/.flick/contexts/`)
- `--dry-run`, `--model`, `--reasoning` flags

## Migration Impact on Epic

Epic currently invokes Flick as a fire-and-forget subprocess (query in → final result out). After this change, Epic must implement a driver loop:

```
context_hash = None
loop:
    if context_hash is None:
        result = json.parse(flick run --config ... --query "do X")
    else:
        result = json.parse(flick run --config ... --resume {context_hash} --tool-results results.json)
    context_hash = result.context_hash
    if result.status == "complete":
        break
    if result.status == "error":
        handle error
        break
    tool_results = []
    for block in result.content where block.type == "tool_call":
        output = execute(block.tool_name, block.arguments)
        tool_results.append({tool_use_id: block.call_id, content: output, is_error: false})
    write tool_results to results.json
```

This is a net gain for Epic — it gains full control over tool execution, sandboxing, approval, and orchestration.

## Estimated Scope

**Deletions:** ~1,700 lines (tool.rs, sandbox.rs, related config/tests)
**Modifications:** ~300 lines (agent.rs simplification, config.rs tool schema, event.rs cleanup, main.rs CLI flags, init command)
**Net:** Significant reduction in code and complexity.

## Output Schema Reference

Flick writes a single JSON object to stdout. One invocation = one object.

### Success result

```json
{
  "status": "tool_calls_pending",
  "content": [
    {"type": "thinking", "text": "...", "signature": "..."},
    {"type": "text", "text": "Here's what I'll do..."},
    {"type": "tool_call", "call_id": "tc_1", "tool_name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}"},
    {"type": "tool_call", "call_id": "tc_2", "tool_name": "shell_exec", "arguments": "{\"command\":\"ls\"}"}
  ],
  "usage": {
    "input_tokens": 1200,
    "output_tokens": 340,
    "cache_creation_input_tokens": 800,
    "cache_read_input_tokens": 400,
    "cost_usd": 0.0087
  },
  "context_hash": "00a1b2c3d4e5f67890abcdef12345678"
}
```

### Final result (no pending tool calls)

```json
{
  "status": "complete",
  "content": [
    {"type": "text", "text": "Done. The file has been updated."}
  ],
  "usage": {
    "input_tokens": 2400,
    "output_tokens": 50,
    "cost_usd": 0.0032
  },
  "context_hash": "11b2c3d4e5f67890abcdef1234567899"
}
```

### Error result

```json
{
  "status": "error",
  "error": {
    "message": "Rate limit exceeded",
    "code": "rate_limit"
  }
}
```

### Field reference

| Field | Type | Description |
|-------|------|-------------|
| `status` | `"complete"` \| `"tool_calls_pending"` \| `"error"` | Outcome of this invocation |
| `content` | array | Ordered content blocks from the model response |
| `content[].type` | `"text"` \| `"thinking"` \| `"tool_call"` | Block type |
| `usage` | object | Token counts and cost (omitted on error) |
| `usage.cache_creation_input_tokens` | number | Omitted when zero |
| `usage.cache_read_input_tokens` | number | Omitted when zero |
| `context_hash` | string | Hash of persisted context file (omitted on error) |
| `error` | object | Error details (only when `status` is `"error"`) |
| `error.code` | string | Machine-readable error code |

---

## Implementation Plan

### Strategy

Single coordinated refactor across 9 steps. Steps 1–2 are additive (code compiles throughout). Steps 3–8 form one atomic change (code may not compile between them). Step 9 is test cleanup. The boundary between "compiles" and "atomic change" is after step 2.

### Step 1: Add result struct (`src/result.rs`) — additive

Create `src/result.rs` with the `FlickResult` type that replaces the event system.

```rust
#[derive(Serialize)]
pub struct FlickResult {
    pub status: ResultStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResultError>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Complete,
    ToolCallsPending,
    Error,
}

#[derive(Serialize)]
pub struct UsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_creation_input_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Serialize)]
pub struct ResultError {
    pub message: String,
    pub code: String,
}
```

Reuses `ContentBlock` from `context.rs` directly — no new content block types needed. The existing `ContentBlock` enum already has `Text`, `Thinking`, `ToolUse`, `ToolResult` variants. The output schema's `tool_call` type maps to the `ToolUse` variant (rename via serde if needed, or keep `tool_use` — decide during implementation).

**Files:** Create `src/result.rs`, add `pub mod result;` to `src/lib.rs`.

**Dependencies:** None. Pure addition.

### Step 2: Add tool result loading to `context.rs` — additive

Add a function to load and parse the `--tool-results` JSON file:

```rust
pub fn load_tool_results(path: &Path) -> Result<Vec<ContentBlock>, FlickError>
```

Reads the JSON array, validates each entry has `tool_use_id` and `content`, converts to `ContentBlock::ToolResult` variants. Also add `Context::push_tool_results_from_file()` or equivalent that wraps the loaded results in a `Message { role: User, content: [...] }` and appends to the message list.

**Files:** Edit `src/context.rs`.

**Dependencies:** None. Pure addition.

### Step 3: Simplify config (`src/config.rs`) — breaking

Remove the following types and all associated validation:
- `SandboxConfig` struct (lines 149–166) and `validate_sandbox()` (lines 357–446)
- `ResourceConfig` struct (lines 130–134) and `ResourceAccess` enum (lines 136–141)
- `BuiltinTool` toggles from `ToolsConfig` (`read_file`, `write_file`, `list_directory`, `shell_exec` bools)
- `command` and `executable` fields from `CustomToolConfig`
- Tool name collision validation against builtin names (no builtins anymore)
- `command`-xor-`executable` validation

Restructure `ToolsConfig`. The current structure:

```rust
// Before
pub struct ToolsConfig {
    read_file: bool,
    write_file: bool,
    list_directory: bool,
    shell_exec: bool,
    custom: Vec<CustomToolConfig>,
}
```

Becomes a flat array of tool definitions:

```rust
// After — config deserializes [[tools]] array directly
pub struct ToolConfig {
    pub name: String,
    pub description: String,
    pub parameters: Option<serde_json::Value>,
}
```

The `Config` struct changes its tools field from `ToolsConfig` to `Vec<ToolConfig>`. The getter `tools()` returns `&[ToolConfig]`.

Add a method to convert `Vec<ToolConfig>` → `Vec<ToolDefinition>` (the provider type), replacing what `ToolRegistry::definitions()` did:

```rust
impl ToolConfig {
    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.parameters.clone(),
        }
    }
}
```

Remaining validation:
- Tool name not empty
- No duplicate tool names

Remove all sandbox/resource getters and config sections: `sandbox()`, `resources()`.

**Files:** Edit `src/config.rs`. Heavy deletions.

**What breaks:** `src/tool.rs`, `src/sandbox.rs`, `src/agent.rs`, `src/main.rs` — all reference removed types.

### Step 4: Delete `src/tool.rs` and `src/sandbox.rs`

Delete both files entirely. Remove `pub mod tool;` and `pub mod sandbox;` from `src/lib.rs`.

**Files:** Delete `src/tool.rs` (1,320 lines), delete `src/sandbox.rs` (701 lines). Edit `src/lib.rs`.

### Step 5: Delete `src/event.rs`

Delete the file. Remove `pub mod event;` from `src/lib.rs`.

**Files:** Delete `src/event.rs` (412 lines). Edit `src/lib.rs`.

### Step 6: Rewrite `src/agent.rs` — single model call

Replace the 25-iteration loop with a single model call that returns `FlickResult`.

New signature:

```rust
pub async fn run(
    config: &Config,
    provider: &dyn DynProvider,
    context: &mut Context,
) -> Result<FlickResult, FlickError>
```

Implementation:
1. Convert `config.tools()` → `Vec<ToolDefinition>` via `ToolConfig::to_definition()`
2. Build `RequestParams` via `build_params()` (unchanged)
3. Call `provider.call_boxed(params).await?`
4. Extract content blocks from response (text, thinking, tool uses)
5. Append assistant message to context
6. Compute cost from token counts
7. Determine status: if any `ToolUse` blocks → `ToolCallsPending`, else → `Complete`
8. Return `FlickResult` with content, usage, status (context hash computed by caller)

`build_params()` stays public — still needed for `--dry-run`.

The `ToolRegistry` parameter is gone. Tool definitions come from `config.tools()` directly.

The `EventEmitter` parameter is gone. No events to emit.

**Files:** Edit `src/agent.rs`. Rewrite from ~206 lines to ~80 lines.

### Step 7: Rewrite `src/main.rs` run path

#### CLI flags

Replace:
```rust
// Before
#[arg(long)] context: Option<PathBuf>,
#[arg(long)] raw: bool,
```

With:
```rust
// After
#[arg(long)] resume: Option<String>,
#[arg(long)] tool_results: Option<PathBuf>,
```

Add validation:
- `--resume` and `--tool-results` must both be present or both absent
- `--query` and `--resume` are mutually exclusive

Remove `RunMode::Raw`. Only two modes remain: `Json` (normal) and `DryRun`.

#### Dispatch logic

**New session** (`--query`):
1. Load config, create provider
2. Create empty context, push user query
3. Call `agent::run(config, provider, &mut context)`
4. Compute context hash, write context file
5. Serialize `FlickResult` (with context_hash set) to stdout
6. Record history

**Resume session** (`--resume` + `--tool-results`):
1. Load config, create provider
2. Load context from `~/.flick/contexts/{hash}.json`
3. Load tool results from `--tool-results` file
4. Append tool results as user message to context
5. Call `agent::run(config, provider, &mut context)`
6. Compute context hash, write context file
7. Serialize `FlickResult` (with context_hash set) to stdout
8. Record history

**Error output:**
Errors serialize as `FlickResult { status: Error, error: Some(...) }` to stdout (not stderr). Exit code 1.

Remove all `EventEmitter` construction. Remove `RawEmitter`, `JsonLinesEmitter` imports. Remove `ToolRegistry` construction. Remove sandbox setup code (wrapper validation, policy generation, `SandboxCommandRunner`).

**Files:** Edit `src/main.rs`. Significant rewrite of `cmd_run()` and `cmd_run_core()`.

### Step 8: Rewrite `flick init` in `src/main.rs`

Remove from `cmd_init_core()`:
- Step 5 (builtin tool selection — `multi_select` for read_file/write_file/etc.)
- `shell_exec` confirmation prompt
- All sandbox-related prompts (none exist currently, but guard against future)

Update config generation:
- Remove `[tools]` section with builtin booleans
- Append commented-out `[[tools]]` template to generated config

Remaining init flow (4 steps):
1. Provider selection
2. Model selection
3. Max output tokens
4. System prompt

Update `ConfigGenParams` to remove tool fields. Update `generate_config_toml()`.

**Files:** Edit `src/main.rs` (init section, lines ~398–545 and config generation helpers ~548–688).

### Step 9: Update `src/history.rs`

Remove `raw: bool` from the `Invocation` struct — the `--raw` flag no longer exists.

Remove `context_path: Option<String>` if it referred to the old `--context` flag. Add `resume_hash: Option<String>` to track whether this was a resume invocation.

Update `record()` call sites in `main.rs`.

**Files:** Edit `src/history.rs`, edit `src/main.rs` (history recording).

### Step 10: Update tests

#### Tests to delete entirely

| File | Tests | Reason |
|------|-------|--------|
| `src/tool.rs` | All (file deleted) | Tool execution removed |
| `src/sandbox.rs` | All 24 tests (file deleted) | Sandbox removed |
| `src/event.rs` | All 15 tests (file deleted) | Event system removed |

#### Tests to rewrite

| Location | Current | After |
|----------|---------|-------|
| `src/main.rs` init tests (21) | Test builtin tool selection, shell_exec confirm, config generation with tools | Remove tool selection tests, update config generation assertions |
| `src/main.rs` run tests (2) | Test dry-run with `ToolRegistry`, emitter construction | Rewrite for new `FlickResult` output, no registry |
| `src/config.rs` tests | Test `ToolsConfig` with builtins, `CustomToolConfig` validation, `SandboxConfig` validation | Rewrite for `Vec<ToolConfig>` parsing, remove sandbox tests |
| `tests/agent.rs` (12) | Test agent loop with mock provider and `ToolRegistry` | Rewrite for single-call agent, no tool execution |
| `tests/integration.rs` (12) | Test end-to-end with tool execution, JSON-lines output | Rewrite for single JSON output, no tool execution |
| `tests/common/mod.rs` | Helper functions for mock tools | Remove tool-related helpers |

#### New tests to add

| Location | Test |
|----------|------|
| `src/result.rs` | `FlickResult` serialization: complete, tool_calls_pending, error variants; `UsageSummary` zero-field omission |
| `src/context.rs` | `load_tool_results()`: valid input, missing fields, empty array, malformed JSON |
| `src/agent.rs` | Single call returning complete; single call returning tool_calls_pending; provider error propagation |
| `src/main.rs` | Resume validation (--resume without --tool-results, --query with --resume); new session end-to-end; resume end-to-end |
| `src/config.rs` | `[[tools]]` array parsing; `ToolConfig::to_definition()` conversion; empty tools array; duplicate name rejection |

### File Change Summary

| File | Action | Lines removed (approx) | Lines added (approx) |
|------|--------|----------------------|---------------------|
| `src/tool.rs` | Delete | 1,320 | 0 |
| `src/sandbox.rs` | Delete | 701 | 0 |
| `src/event.rs` | Delete | 412 | 0 |
| `src/result.rs` | Create | 0 | 80 |
| `src/agent.rs` | Rewrite | 180 | 70 |
| `src/config.rs` | Heavy edit | 500 | 40 |
| `src/main.rs` | Heavy edit | 600 | 200 |
| `src/context.rs` | Edit | 0 | 40 |
| `src/history.rs` | Edit | 10 | 10 |
| `src/lib.rs` | Edit | 3 | 1 |
| `tests/agent.rs` | Rewrite | 200 | 80 |
| `tests/integration.rs` | Rewrite | 200 | 80 |
| `tests/common/mod.rs` | Edit | 50 | 10 |
| **Total** | | **~4,176** | **~611** |

**Net reduction: ~3,565 lines.**

### Execution Order Constraints

```
Step 1 (result.rs)        — independent, additive
Step 2 (context.rs)       — independent, additive
Step 3 (config.rs)        — breaks tool.rs, sandbox.rs, agent.rs, main.rs
Step 4 (delete tool+sandbox) — requires step 3
Step 5 (delete event.rs)  — requires step 7 (main.rs must stop importing events first)
Step 6 (agent.rs)         — requires steps 1, 3, 4
Step 7 (main.rs run)      — requires steps 1, 2, 3, 4, 6
Step 8 (main.rs init)     — requires step 3
Step 9 (history.rs)       — requires step 7
Step 10 (tests)           — requires all above
```

Practical ordering: 1 → 2 → 3 → 4 → 6 → 7 → 5 → 8 → 9 → 10.

Steps 1 and 2 can be done in parallel. Steps 5 and 8 can be done in parallel (after step 7).

### Documents to Update After Implementation

- `docs/OVERVIEW.md` — CLI interface, output format, agent loop, design principles
- `docs/STATUS.md` — module summary, next work, test counts
- `docs/ARCHITECTURE.md` — module descriptions, data flow
- `docs/CONFIGURATION.md` — tool config reference, remove sandbox/resource sections
- `docs/SANDBOX.md` — mark phases B and C as cancelled; note phase A removed
- `docs/INIT_COMMAND.md` — remove tool selection and sandbox steps
