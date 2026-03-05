# Flick

Ultra-small, ultra-fast LLM primitive written in Rust. Available as both a **CLI tool** (`flick-cli`) and a **Rust library** (`flick`). Takes a YAML (or JSON) config and a query, makes a single LLM call, and returns a JSON result. Flick declares tool definitions to the model but never executes tools. The caller drives the agent loop externally.

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

1. Store an API key:

```sh
flick setup anthropic
```

2. Create a config file (`config.yaml`):

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-20250514
  max_tokens: 8192

system_prompt: "You are a helpful assistant."

provider:
  anthropic:
    api: messages
```

3. Run a query:

```sh
flick run --config config.yaml --query "What is Rust?"
```

## Library Usage

Add `flick` as a dependency:

```toml
[dependencies]
flick = { path = "flick" }  # or from your registry
tokio = { version = "1", features = ["rt", "macros"] }
```

```rust
use flick::{Config, ConfigFormat, Context, FlickClient};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = std::fs::read_to_string("config.yaml")?;
    let config = Config::from_str(&yaml, ConfigFormat::Yaml)?;
    let provider = flick::resolve_provider(&config).await?;
    let client = FlickClient::new(config, provider);

    let mut ctx = Context::default();
    let result = client.run("What is Rust?", &mut ctx).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);

    // To resume after tool calls:
    // let result = client.resume(&mut ctx, tool_results).await?;
    Ok(())
}
```

For callers that manage their own credentials, use `flick::provider::create_provider` directly to construct a provider, then pass it to `FlickClient::new`.

## CLI Reference

```
flick run --config <file> [OPTIONS]
flick setup <provider>
flick init [--output <path>]
flick list
```

### `flick run`

| Flag | Description |
|------|-------------|
| `--config <path>` | Path to config file (.yaml, .yml, or .json) (required) |
| `--query <text>` | Query text; reads from stdin if omitted |
| `--resume <hash>` | Resume a previous session by context hash |
| `--tool-results <path>` | JSON file containing tool results for resumed session |
| `--dry-run` | Dump API request as JSON without calling the model |
| `--model <id>` | Override model ID from config |
| `--reasoning <level>` | Override reasoning level (`minimal`, `low`, `medium`, `high`) |

Validation:
- `--resume` and `--tool-results` must both be present or both absent.
- `--query` and `--resume` are mutually exclusive.

### `flick init`

Interactive config generator. Walks through provider selection, model, max output tokens, and system prompt, then writes a commented YAML config file.

| Flag | Default | Description |
|------|---------|-------------|
| `--output <path>` | `flick.yaml` | Output file path (use `-` for stdout) |

### `flick setup`

Interactive credential onboarding. Prompts for an API key, API type, and base URL, then stores them encrypted at `~/.flick/credentials`.

### `flick list`

Lists onboarded providers in tab-separated columns (name, API type, base URL), sorted alphabetically. Produces no output if no credentials exist.

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

The `usage` fields `cache_creation_input_tokens` and `cache_read_input_tokens` are omitted when zero.

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

Flick is configured via a YAML file (or JSON for machine-generated configs). Format is detected by file extension (`.yaml`, `.yml`, `.json`).

Full example:

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-20250514
  max_tokens: 8192
  temperature: 0.0
  reasoning:
    level: medium

system_prompt: "You are a code assistant."

output_schema:
  schema:
    type: object
    properties:
      answer:
        type: string

provider:
  anthropic:
    api: messages
    credential: anthropic

  openrouter:
    api: chat_completions
    credential: openrouter
    compat:
      explicit_tool_choice_auto: true

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

pricing:
  input_per_million: 3.0
  output_per_million: 15.0
```

### `model`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `provider` | string | yes | — | Must match a key under `provider:` |
| `name` | string | yes | — | Model identifier |
| `max_tokens` | u32 | no | none | Maximum output tokens (must be > 0); omitted = provider default |
| `temperature` | f32 | no | none | Sampling temperature; omitted for reasoning models. Messages API: 0.0–1.0, Chat Completions: 0.0–2.0 |

### `model.reasoning`

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

### `provider.<name>`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `api` | string | yes | — | `messages` or `chat_completions` |
| `credential` | string | no | provider name | Key name in credential store |

### `provider.<name>.compat`

Compatibility flags for Chat Completions providers:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `explicit_tool_choice_auto` | bool | false | Send `tool_choice: "auto"` explicitly |

### `tools`

Declare tool schemas. Flick includes these in the model request but never executes tools — the caller handles execution.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Tool name (must be unique) |
| `description` | string | yes | Description sent to the model |
| `parameters` | JSON value | no | JSON Schema for tool parameters |

### `pricing`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `input_per_million` | f64 | yes | Cost per million input tokens (USD, non-negative) |
| `output_per_million` | f64 | yes | Cost per million output tokens (USD, non-negative) |

Optional. Overrides the builtin model registry pricing. Cost is reported in the `usage` field of the result.

## Run History

After each successful (non-dry-run) invocation, Flick records:

- **`~/.flick/history.jsonl`** — one JSON object per line capturing timestamp, invocation args, token usage, cost, and a context hash.
- **`~/.flick/contexts/{hash}.json`** — the full conversation context, keyed by its xxh3-128 hash (content-addressable dedup — identical contexts are stored once).

History writes are non-fatal. Failures produce a stderr warning without affecting the exit code or output.

## Context Resumption

Resume a session by passing `--resume` with the context hash and `--tool-results` with a JSON file:

```sh
flick run --config config.yaml --resume 00a1b2c3d4e5f67890abcdef12345678 --tool-results results.json
```

The tool results file contains an array of results:

```json
[
  {"tool_use_id": "tc_1", "content": "file contents here", "is_error": false},
  {"tool_use_id": "tc_2", "content": "command not found", "is_error": true}
]
```

## Provider Support

| API Type | Providers |
|----------|-----------|
| **Messages API** (native) | Anthropic (Claude) |
| **Chat Completions** | OpenAI, OpenRouter, Groq, Mistral, Ollama, DeepSeek, etc. |

### Provider Examples

Anthropic:

```yaml
provider:
  anthropic:
    api: messages
```

OpenAI:

```yaml
provider:
  openai:
    api: chat_completions
```

OpenRouter:

```yaml
provider:
  openrouter:
    api: chat_completions
    compat:
      explicit_tool_choice_auto: true
```

Ollama (local):

```yaml
provider:
  ollama:
    api: chat_completions
```

## Credential Store

Credentials are stored at `~/.flick/credentials` (encrypted with ChaCha20-Poly1305). A 256-bit secret key is generated on first use and stored at `~/.flick/.secret_key` with restrictive file permissions.

```sh
# Store a credential
flick setup anthropic

# List stored credentials
flick list

# Credentials are referenced by name in config:
# provider:
#   anthropic:
#     credential: anthropic   # matches the name passed to `flick setup`
```

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

333 tests (248 lib, 56 bin, 18 runner, 11 integration). One additional Unix-only test for file permissions.

## License

MIT — see [LICENSE](LICENSE) for details.
