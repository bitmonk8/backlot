# Reel CLI Tool Design

## Overview

Reel follows the same dual-nature pattern as Flick: a library crate (`reel`) and a thin CLI
binary crate (`reel-cli`, binary name `reel`). The CLI provides a command-line interface to the
reel library API, handling argument parsing and stdio formatting while delegating all core logic
to the library.

## Relationship to Flick

Flick's CLI exposes: `run`, `provider add/list`, `model add/list/remove`, `init`.

Reel sits one layer above Flick. It consumes Flick's `ModelRegistry` and `ProviderRegistry`
(re-exported from the reel library for constructing `AgentEnvironment`) and adds the agent tool
loop, sandboxed NuShell execution, and built-in tools.

The Reel CLI only exposes commands that map to the reel library's public API. Provider and model
management are not part of reel's API — users configure those via the Flick CLI. Reel uses the
flick library directly (not by launching flick processes) for performance, but does not duplicate
flick's configuration surface.

## Config Format

Reel's config file is a **superset of Flick's `RequestConfig`**. It contains all flick config
fields (model, system_prompt, temperature, reasoning, output_schema) plus reel-specific fields
(grant). The `AgentRequestConfig` struct wraps a `RequestConfig` rather than duplicating its fields.

```yaml
# Reel agent configuration

# --- Flick RequestConfig fields (passed through to FlickClient) ---

# Model key from ~/.flick/models (required)
model: "fast"

# System prompt (required for agent use)
system_prompt: "You are a code assistant."

# Temperature (optional, default: not set — uses provider default)
# temperature: 0.0

# Reasoning budget (optional)
# reasoning:
#   level: medium  # minimal (1k), low (4k), medium (10k), high (32k)

# Structured output schema (optional)
# output_schema:
#   schema:
#     type: object
#     properties:
#       summary:
#         type: string
#     required: [summary]

# --- Reel-specific fields ---

# Tool grants: which built-in tools the agent may use.
# Possible values: "write", "nu". Omit or leave empty for read-only tools.
grant:
  - nu
  - write
```

### How it works

Reel's config is parsed in two passes:

1. **Reel pass**: Extract reel-specific fields (`grant`). These do not exist in flick's config.
2. **Flick pass**: The remaining fields are parsed as a `RequestConfig` by flick.

Flick's `RequestConfig` uses `serde(deny_unknown_fields)`, which would reject reel's extra
fields. Reel handles this by stripping its own fields (`grant`) from the parsed YAML before
passing the remainder to flick's parser. This preserves flick's strict validation for its own
CLI while letting reel layer on top.

### Mapping to library types

`AgentRequestConfig` wraps a flick `RequestConfig`:

```rust
pub struct AgentRequestConfig {
    /// Flick request config (model, system_prompt, temperature, reasoning,
    /// output_schema). Reel injects built-in tool definitions into this
    /// before passing it to FlickClient.
    pub config: flick::RequestConfig,

    /// Tool grant controlling which built-in tools are available.
    pub grant: ToolGrant,

    /// Consumer-provided tools beyond the built-ins.
    pub custom_tools: Vec<Box<dyn ToolHandler>>,
}
```

The query is not part of the config — it is a per-invocation input passed as a method argument:

```rust
impl Agent {
    pub fn new(env: AgentEnvironment) -> Self;
    pub async fn run<T>(&self, request: &AgentRequestConfig, query: &str) -> Result<RunResult<T>>;
}
```

`Agent` is a wrapper around an `AgentEnvironment`. It holds the resolved registries, project
root, and timeout. `run` takes a reference to the request config (reusable across calls) and
the query string (varies per invocation).

### Required flick changes

1. **`RequestConfig::add_tools(&mut self, tools: Vec<ToolConfig>)`** — Reel needs to inject
   built-in tool definitions (Read, Write, Edit, Glob, Grep, NuShell) into the config after
   loading it. Currently there is no way to mutate tools on an existing `RequestConfig`. The
   builder can set tools at construction time, but reel loads configs from files and needs to
   add tools post-parse. This replaces the current `build_request_config` hack in `agent.rs`
   that serializes to JSON and re-parses.

2. **`ToolConfig` constructor or builder** — Reel constructs tool definitions programmatically.
   `ToolConfig` fields are currently private with no public constructor. Either:
   - Add `ToolConfig::new(name, description, parameters)`, or
   - Make `ToolConfig` fields public, or
   - Add a `ToolConfigBuilder`.

### Required reel library changes

1. **Rename `AgentRequest` → `AgentRequestConfig`** — Aligns naming with its role as reusable
   request configuration rather than a single-use request.

2. **Restructure to wrap `flick::RequestConfig`** — The current `AgentRequest` duplicates flick
   config fields (`system_prompt`, `model`, `output_schema`) as flat struct members and manually
   plumbs them into a `RequestConfig` at call time. The new `AgentRequestConfig` wraps a
   `RequestConfig` directly (as shown in the struct definition above), which:
   - Eliminates field duplication between reel and flick.
   - Automatically gains `temperature` and `reasoning` support — these exist on `RequestConfig`
     but are not currently plumbed through `AgentRequest`.

3. **Move `query` out of the struct** — Currently `AgentRequest.query` is a struct field. It
   moves to a parameter on `Agent::run()`, separating per-invocation input from reusable config.

4. **Change `Agent::run` to borrow config** — Current signature takes ownership
   (`run(request: AgentRequest)`). New signature takes a reference
   (`run(&self, request: &AgentRequestConfig, query: &str)`), enabling config reuse across
   multiple calls without cloning.

| Config field     | Library type                            | Notes                          |
|------------------|-----------------------------------------|--------------------------------|
| `model`          | `flick::RequestConfig.model`            | Key into `ModelRegistry`       |
| `system_prompt`  | `flick::RequestConfig.system_prompt`    |                                |
| `temperature`    | `flick::RequestConfig.temperature`      |                                |
| `reasoning`      | `flick::RequestConfig.reasoning`        |                                |
| `output_schema`  | `flick::RequestConfig.output_schema`    |                                |
| `grant`          | `AgentRequestConfig.grant`              | Parsed to `ToolGrant` bitflags |
| (query)          | `Agent::run(_, query)` parameter        | From `--query` / stdin         |
| (custom_tools)   | `AgentRequestConfig.custom_tools`       | Library-only (not exposed via CLI) |

## Command Structure

```
reel <COMMAND>

Commands:
  run        Run an agent session
  setup      Configure platform prerequisites
```

### `reel run`

Core command. Creates an `Agent`, executes a query with the tool loop, and prints results to
stdout.

```
reel run --config <PATH> [OPTIONS]

Options:
  --config <PATH>        Path to reel config file (YAML)
  --query <TEXT>          Query text (if omitted, reads from stdin)
  --project-root <PATH>  Working directory for tool execution (default: cwd)
  --timeout <SECONDS>    Per-tool-call timeout (default: 120)
  --dry-run              Build the request and print it without calling the model
```

`--project-root` and `--timeout` are CLI-only flags — they map to `AgentEnvironment`, which is
runtime context rather than request configuration. They are not part of the config file.

**`--dry-run` output:** Prints the fully assembled flick `RequestConfig` as JSON to stdout — with
injected tool definitions, resolved model, system prompt, and all options. This shows exactly what
the model would receive. No model call is made. Exit code 0.

**Behaviour:**
1. Parse reel config from `--config`. Extract `grant` → `ToolGrant`. Parse remaining fields as
   flick `RequestConfig`. Inject built-in tool definitions based on grant.
2. Build `AgentRequestConfig` from `RequestConfig` + grant.
3. Load `ModelRegistry` and `ProviderRegistry` from `~/.flick/`.
4. Construct `AgentEnvironment` from registries, `--project-root`, and `--timeout`.
5. Construct `Agent` from environment. Call `agent.run(&request_config, query)`.
6. Print `RunResult` as JSON to stdout. Include the final flick response hash in the output
   so callers can reference the conversation in flick's history/logs.
7. Exit code 0 on success, 1 on error (error wrapped as JSON on stdout).

**Stdin query:** When `--query` is absent, read query text from stdin until EOF. This supports
piping: `echo "explain this code" | reel run --config agent.yaml`. When both `--query` and
stdin are provided, `--query` wins and stdin is ignored silently. When stdin is a TTY (no pipe),
print `Reading query from stdin (Ctrl+D to submit)...` to stderr before blocking on read.
Detected via `std::io::stdin().is_terminal()`.

### `reel setup`

Platform prerequisite configuration. Currently Windows-only (AppContainer ACLs).

```
reel setup [OPTIONS]

Options:
  --check    Check prerequisites without modifying anything (exit 0 if OK, 1 if not)
  --verbose  Print details of what is being configured
```

**Behaviour (Windows):**
1. Grant AppContainer access to the NUL device.
2. Add ancestor directory traverse ACEs required by sandboxed processes.
3. ACL logic extracted/adapted from `epic setup` at `C:\UnitySrc\epic` (sibling project),
   scoped to reel's needs.

**Behaviour (non-Windows):** Print "No setup required on this platform." and exit 0.

This command addresses the issue identified in `CLI_TOOL_INTEGRATION_TESTS.md` — sandboxed tool
tests fail without these ACLs.

## Architecture

```
┌─────────────────────────────────────────────┐
│  reel-cli (binary)                          │
│  - clap argument parsing                    │
│  - stdin/stdout/stderr formatting           │
│  - tokio runtime bootstrap                  │
│  - config parsing (reel fields + flick      │
│    passthrough)                              │
│                                             │
│  Calls into:                                │
├─────────────────────────────────────────────┤
│  reel (library)                             │
│  - Agent (wraps AgentEnvironment)           │
│  - AgentRequestConfig wraps flick::ReqCfg   │
│  - NuSession (sandboxed nushell)            │
│  - Tools (read, write, edit, glob, grep)    │
│  - Re-exports: ModelRegistry,               │
│    ProviderRegistry from flick (read-only)  │
├─────────────────────────────────────────────┤
│  flick (library)                            │
│  - FlickClient, RequestConfig, Context      │
│  - Provider system (Messages, ChatCompl.)   │
│  - Model/Provider registries                │
└─────────────────────────────────────────────┘
```

## Dependencies (reel-cli)

- `reel` — the library crate (workspace dependency)
- `clap` — argument parsing (derive macros)
- `tokio` — async runtime (current_thread)
- `serde` / `serde_json` / `serde_yml` — config parsing and JSON output

No other dependencies. All LLM, sandboxing, and tool logic lives in the library.

## Data Directory

Reel has no data directory of its own. Conversation history is tracked via flick's existing
history/logs system — reel exposes the final flick response hash in its output for reference.

Reel reads (but does not write) Flick's directories:
- `~/.flick/providers` — encrypted provider credentials (managed by `flick provider`)
- `~/.flick/models` — model definitions (managed by `flick model`)

## Output Format

All commands that produce structured output use JSON on stdout. Human-readable messages go to
stderr. This supports piping and programmatic consumption:

```sh
reel run --config agent.yaml --query "list files" | jq '.content'
```

**Success shape (`reel run`):**

```json
{
  "status": "Ok",
  "content": "The agent's final text response",
  "usage": { "input_tokens": 1234, "output_tokens": 567, "cost_usd": 0.02 },
  "tool_calls": 12,
  "response_hash": "abc123"
}
```

`content` is a string when no `output_schema` is configured. When `output_schema` is set,
`content` is the schema-shaped JSON object returned by the model. The CLI resolves `RunResult<T>`
by using `T = String` for free-form and `T = serde_json::Value` for structured output.

## Error Handling

Errors are reported as JSON on stdout with exit code 1, matching Flick's convention:

```json
{
  "status": "Error",
  "error": { "code": "config_error", "message": "Model 'gpt-5' not found in registry" }
}
```

The CLI never panics on external input (bad config, missing files, network errors, malformed
responses). These are caught and formatted as JSON errors. Panics are acceptable only for
violated internal invariants (logic bugs) that the type system cannot enforce.


