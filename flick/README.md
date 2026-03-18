# Flick

Ultra-small, ultra-fast LLM primitive written in Rust. Available as both a **CLI tool** (`flick-cli`) and a **Rust library** (`flick`). Takes a YAML (or JSON) request config and a query, makes a single LLM call, and returns a JSON result. Flick declares tool definitions to the model but never executes tools. The caller drives the agent loop externally.

The project is a Cargo workspace with two crates:

| Crate | Type | Description |
|-------|------|-------------|
| `flick` | library | Core engine — config parsing, provider abstraction, model calling |
| `flick-cli` | binary | CLI interface wrapping the library |

## Relationship to Epic

| Project | Role |
|---------|------|
| Epic | Orchestrator — recursive task decomposition, tool execution, state management, TUI |
| Flick | Agent primitive — single-shot LLM call, tool declaration (not execution), JSON result output |

## Design Principles

- **Ultra-small.** Minimal binary, minimal dependencies (13 runtime crates (+1 Windows-only)).
- **Ultra-fast.** Negligible startup overhead. Time-to-first-token is the bottleneck.
- **Unix-philosophy.** Takes input, produces output, composes via stdin/stdout.
- **Dual interface.** Usable as a standalone CLI or embedded as a Rust library.
- **Tool-calling models only.** No capability-checking fallbacks.
- **Compatibility-by-configuration.** Provider quirks via flags, not subclasses.
- **Separation of concerns.** Flick is a pure LLM interface: config in, model call, result out. Tool execution is the caller's responsibility.
- **Monadic / single-shot.** One invocation = one model call = one JSON result. The caller composes invocations into an agent loop.

## Requirements

- Rust 1.85+ (edition 2024)

## Build

```sh
cargo build --release
```

The release binary is optimized with LTO, single codegen unit, and symbol stripping.

## Quick Start

1. Register a provider:

```sh
flick provider add anthropic
```

2. Register a model:

```sh
flick model add balanced
```

3. Create a request config file (`flick.yaml`):

```yaml
model: balanced
system_prompt: "You are a helpful assistant."
```

Or generate one interactively:

```sh
flick init
```

4. Run a query:

```sh
flick run --config flick.yaml --query "What is Rust?"
```

## Provider Registry

Providers are stored at `~/.flick/providers` (TOML, encrypted with ChaCha20-Poly1305). A 256-bit secret key is generated on first use and stored at `~/.flick/.secret_key` with restrictive file permissions.

```sh
# Add a provider
flick provider add anthropic

# List providers
flick provider list
```

## Model Registry

Models are stored at `~/.flick/models` (TOML). Each entry maps a user-chosen name to a provider reference, model ID, max_tokens, and optional pricing (input, output, cache creation, cache read — all per million tokens).

```sh
# Add a model
flick model add balanced

# List models
flick model list

# Remove a model
flick model remove balanced
```

No builtin models. The registry is empty until the user runs `flick model add`.

## Library Usage

Add `flick` as a dependency:

```toml
[dependencies]
flick = { path = "flick" }  # or from your registry
tokio = { version = "1", features = ["rt", "macros"] }
```

```rust
use flick::{RequestConfig, ConfigFormat, ModelRegistry, ProviderRegistry, FlickClient, Context};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load registries (once at startup)
    let providers = ProviderRegistry::load_default()?;
    let models = ModelRegistry::load_default().await?;

    // Parse request config
    let yaml = std::fs::read_to_string("flick.yaml")?;
    let request = RequestConfig::from_str(&yaml, ConfigFormat::Yaml)?;

    // Build client (resolves model -> provider chain)
    let client = FlickClient::new(request, &models, &providers).await?;

    let mut ctx = Context::default();
    let result = client.run("What is Rust?", &mut ctx).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);

    // To resume after tool calls:
    // let result = client.resume(&mut ctx, tool_results).await?;
    Ok(())
}
```

For library consumers switching models across calls:

```rust
let providers = ProviderRegistry::load_default()?;
let models = ModelRegistry::load_default().await?;

// Fast model call
let request = RequestConfig::builder()
    .model("fast")
    .system_prompt("Triage this issue.")
    .build()?;
let client = FlickClient::new(request, &models, &providers).await?;

// Strong model call
let request = RequestConfig::builder()
    .model("strong")
    .system_prompt("Write a detailed implementation plan.")
    .tools(planning_tools)
    .build()?;
let client = FlickClient::new(request, &models, &providers).await?;
```

## CLI Reference

```
flick run --config <file> [OPTIONS]
flick provider add <name>
flick provider list
flick model add <name>
flick model list
flick model remove <name>
flick init [--output <path>]
```

### `flick run`

| Flag | Description |
|------|-------------|
| `--config <path>` | Path to config file (.yaml, .yml, or .json) (required) |
| `--query <text>` | Query text; reads from stdin if omitted |
| `--resume <hash>` | Resume a previous session by context hash |
| `--tool-results <path>` | JSON file containing tool results for resumed session |
| `--dry-run` | Dump API request as JSON without calling the model |

Validation:
- `--resume` and `--tool-results` must both be present or both absent.
- `--query` and `--resume` are mutually exclusive.

### `flick provider add`

Interactive provider onboarding. Prompts for an API key, API type, and base URL, then stores them encrypted at `~/.flick/providers`.

### `flick provider list`

Lists providers in tab-separated columns (name, API type, base URL), sorted alphabetically.

### `flick model add`

Interactive model onboarding. Prompts for provider, model ID, max_tokens, and pricing (input, output, cache creation, cache read — all per million tokens). Writes to `~/.flick/models`.

### `flick model list`

Lists models in tab-separated columns (key, provider, model ID, max_tokens).

### `flick model remove`

Removes a model entry from `~/.flick/models`.

### `flick init`

Interactive config generator. Selects a model from the ModelRegistry and a system prompt, then writes a RequestConfig YAML file. If the ModelRegistry is empty, directs user to `flick model add` first.

| Flag | Default | Description |
|------|---------|-------------|
| `--output <path>` | `flick.yaml` | Output file path (use `-` for stdout) |

## Output Format

Each invocation writes one JSON object to stdout. The `status` field tells the caller what to do next.

**Tool calls pending** (caller must execute tools and resume):
```json
{
  "status": "tool_calls_pending",
  "content": [
    {"type": "text", "text": "I'll read that file."},
    {"type": "tool_use", "id": "tc_1", "name": "read_file", "input": {"path": "src/main.rs"}}
  ],
  "usage": {"input_tokens": 1200, "output_tokens": 340, "cache_creation_input_tokens": 800, "cache_read_input_tokens": 400, "cost_usd": 0.0087},
  "context_hash": "00a1b2c3d4e5f67890abcdef12345678"
}
```

**Complete** (no further action):
```json
{
  "status": "complete",
  "content": [{"type": "text", "text": "Done."}],
  "usage": {"input_tokens": 2400, "output_tokens": 50, "cost_usd": 0.0032},
  "context_hash": "11b2c3d4e5f67890abcdef1234567899"
}
```

**Error:**
```json
{"status": "error", "error": {"message": "Rate limit exceeded", "code": "rate_limit"}}
```

The `usage` fields `cache_creation_input_tokens` and `cache_read_input_tokens` are omitted when zero. The `cost_usd` field includes cache token costs when `cache_creation_per_million` and `cache_read_per_million` are configured in the model registry.

## Invocation Model

Each `flick run` makes exactly one model call and returns. The caller drives the loop:

1. Call provider with message history
2. Append assistant message to context
3. Write context file, compute hash
4. Return JSON result with `status`:
   - `tool_calls_pending` — caller executes tools, resumes with `--resume <hash> --tool-results <file>`
   - `complete` — session finished
   - `error` — invocation failed

## Configuration

Flick is configured via a RequestConfig YAML file (or JSON for machine-generated configs). Format is detected by file extension (`.yaml`, `.yml`, `.json`).

Full example:

```yaml
model: balanced
system_prompt: "You are a code assistant."
temperature: 0.0
reasoning:
  level: medium
output_schema:
  schema:
    type: object
    properties:
      answer:
        type: string
tools:
  - name: read_file
    description: "Read a file's contents"
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
  - name: grep_project
    description: Search for a pattern
    parameters:
      type: object
      properties:
        pattern:
          type: string
      required: [pattern]
```

### `model`

String key referencing an entry in the ModelRegistry (`~/.flick/models`).

### `reasoning`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `level` | string | yes | `minimal`, `low`, `medium`, or `high` |

Reasoning levels are mapped per-provider:

| Level | Anthropic (`budget_tokens`) | OpenAI (`reasoning_effort`) |
|-------|----------------------------|----------------------------|
| minimal | 1024 | low |
| low | 4096 | low |
| medium | 10000 | medium |
| high | 32000 | high |

For Anthropic, `budget_tokens` must be less than `max_tokens`. When `max_tokens` is omitted, the model's default max output tokens is used (fallback: 8192). Validated at config load.

### `system_prompt`

Top-level string. Optional system prompt sent to the model.

### `output_schema`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema` | JSON value | yes | JSON Schema for structured output |

Both provider types support structured output. Messages providers send the schema as
`output_config.format` (native `json_schema` mode). Chat Completions providers send
it as `response_format`. When using a Chat Completions provider with both `tools` and
`output_schema`, Flick automatically performs a two-step call: the first request
includes tools (no schema), and if the model completes without tool calls, a second
request applies the schema (no tools). Usage from both calls is summed.

### `tools`

Declare tool schemas. Flick includes these in the model request but never executes tools — the caller handles execution.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Tool name (must be unique) |
| `description` | string | yes | Description sent to the model |
| `parameters` | JSON value | no | JSON Schema for tool parameters |

## Context Resumption

Resume a session by passing `--resume` with the context hash and `--tool-results` with a JSON file:

```sh
flick run --config flick.yaml --resume 00a1b2c3d4e5f67890abcdef12345678 --tool-results results.json
```

The tool results file contains an array of results:

```json
[
  {"tool_use_id": "tc_1", "content": "file contents here", "is_error": false},
  {"tool_use_id": "tc_2", "content": "command not found", "is_error": true}
]
```

## Run History

After each successful (non-dry-run) invocation, Flick records:

- **`~/.flick/history.jsonl`** — one JSON object per line capturing timestamp, invocation args, token usage, cost, and a context hash.
- **`~/.flick/contexts/{hash}.json`** — the full conversation context, keyed by its xxh3-128 hash (content-addressable dedup — identical contexts are stored once).

History writes are non-fatal. Failures produce a stderr warning without affecting the exit code or output.

## Provider Support

| API Type | Providers |
|----------|-----------|
| **Messages API** (native) | Anthropic (Claude) |
| **Chat Completions** | OpenAI, OpenRouter, Groq, Mistral, Ollama, DeepSeek, etc. |

## HTTP Retry

The initial HTTP request uses exponential backoff for transient errors:

- **Retryable:** 429 (rate limit), 5xx (server error), network errors
- **Non-retryable:** 401 (auth), other 4xx (client error)
- **Defaults:** 3 retries, 500ms initial delay, 2x multiplier, 30s cap
- **429 responses:** `Retry-After` header overrides computed backoff

Retry applies only to the HTTP request/response exchange.

## Testing

```sh
cargo test
```

255 tests (202 lib, 22 bin, 20 runner, 11 integration). One additional Unix-only test for file permissions.

## License

MIT — see [LICENSE](LICENSE) for details.
