# Flick

Ultra-small, ultra-fast command-line LLM agent written in Rust. Takes a TOML config and a query, streams typed events to stdout, and executes tools via an agent loop.

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

2. Create a config file (`config.toml`):

```toml
[model]
provider = "anthropic"
name = "claude-sonnet-4-20250514"
max_tokens = 8192

system_prompt = "You are a helpful assistant."

[provider.anthropic]
api = "messages"
```

3. Run a query:

```sh
flick run --config config.toml --query "What is Rust?"
```

## CLI Reference

```
flick run --config <toml> [OPTIONS]
flick setup <provider>
```

### `flick run`

| Flag | Description |
|------|-------------|
| `--config <path>` | Path to TOML config file (required) |
| `--query <text>` | Query text; reads from stdin if omitted |
| `--context <path>` | JSON file with prior message history |
| `--raw` | Plain text output instead of JSON-lines |
| `--dry-run` | Dump API request as JSON without calling the model |
| `--model <id>` | Override model ID from config |
| `--reasoning <level>` | Override reasoning level (`minimal`, `low`, `medium`, `high`) |

### `flick setup`

Interactive credential onboarding. Prompts for an API key and stores it encrypted at `~/.flick/credentials`.

## Streaming Output

By default, Flick emits one JSON object per line to stdout:

```jsonl
{"type":"text_delta","text":"Hello "}
{"type":"thinking_delta","text":"..."}
{"type":"tool_call_start","call_id":"tc_1","tool_name":"read_file"}
{"type":"tool_call_delta","call_id":"tc_1","arguments_delta":"..."}
{"type":"tool_call_end","call_id":"tc_1","arguments":"{...}"}
{"type":"tool_result","call_id":"tc_1","success":true,"output":"..."}
{"type":"thinking_signature","signature":"sig_..."}
{"type":"usage","input_tokens":1200,"output_tokens":340,"cache_creation_input_tokens":800,"cache_read_input_tokens":400}
{"type":"done","usage":{"input_tokens":1200,"output_tokens":340,"cost_usd":0.0087,"iterations":2}}
{"type":"error","message":"...","code":"rate_limit"}
```

The `usage` event's `cache_creation_input_tokens` and `cache_read_input_tokens` fields are omitted when zero.

With `--raw`, only text deltas are printed as plain text. Errors go to stderr.

## Agent Loop

1. Call provider with message history
2. Stream events to stdout, accumulate text and tool calls
3. Append assistant message to history
4. If no tool calls, emit `done` and exit
5. Execute tool calls, emit `tool_result` events
6. Append tool results to history
7. Goto 1 (capped at 25 iterations)

## Configuration

Flick is configured via a TOML file. Full example:

```toml
[model]
provider = "anthropic"
name = "claude-sonnet-4-20250514"
max_tokens = 8192
temperature = 0.0

[model.reasoning]
level = "medium"

system_prompt = "You are a code assistant."

[output_schema]
schema = { type = "object", properties = { answer = { type = "string" } } }

[provider.anthropic]
api = "messages"
base_url = "https://api.anthropic.com"
credential = "anthropic"

[provider.openrouter]
api = "chat_completions"
base_url = "https://openrouter.ai/api"
credential = "openrouter"
[provider.openrouter.compat]
explicit_tool_choice_auto = true

[tools]
read_file = true
write_file = true
list_directory = true
shell_exec = true

[[tools.custom]]
name = "search_codebase"
description = "Search files for a pattern"
parameters = { type = "object", properties = { pattern = { type = "string" } } }
command = "rg --json {{pattern}} {{path}}"

[[tools.custom]]
name = "code_search"
description = "Semantic code search"
parameters = { type = "object", properties = { query = { type = "string" } } }
executable = "./tools/code_search"

[[resources]]
path = "src/"
access = "read_write"
[[resources]]
path = "docs/"
access = "read"

[pricing]
input_per_million = 3.0
output_per_million = 15.0
```

### `[model]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `provider` | string | yes | — | Must match a key in `[provider.*]` |
| `name` | string | yes | — | Model identifier |
| `max_tokens` | u32 | no | 8192 | Maximum output tokens (must be > 0) |
| `temperature` | f32 | no | none | Sampling temperature (0.0–2.0); omitted for reasoning models |

### `[model.reasoning]`

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

For Anthropic, `budget_tokens` must be less than `max_tokens` (validated at config load).

### `system_prompt`

Top-level string. Optional system prompt sent to the model.

### `[output_schema]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema` | JSON value | yes | JSON Schema for structured output (Anthropic only) |

### `[provider.<name>]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `api` | string | yes | — | `"messages"` or `"chat_completions"` |
| `base_url` | string | no | per-API default | API base URL |
| `credential` | string | no | provider name | Key name in credential store |

### `[provider.<name>.compat]`

Compatibility flags for Chat Completions providers:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `explicit_tool_choice_auto` | bool | false | Send `tool_choice: "auto"` explicitly |
| `skip_stream_options` | bool | false | Omit `stream_options` from request |

### `[tools]`

Builtin tools (all default to `false`):

| Tool | Description |
|------|-------------|
| `read_file` | Read file contents |
| `write_file` | Write file contents |
| `list_directory` | List directory entries (capped at 10,000) |
| `shell_exec` | Execute shell commands (120s timeout) |

### `[[tools.custom]]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Tool name (must be unique, no collision with builtins) |
| `description` | string | yes | Description sent to the model |
| `parameters` | JSON value | no | JSON Schema for tool parameters |
| `command` | string | one of | Shell command with `{{param}}` substitution (shell-escaped) |
| `executable` | string | one of | Path to executable (receives JSON on stdin) |

Exactly one of `command` or `executable` is required.

### `[[resources]]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `path` | string | yes | Path to file or directory |
| `access` | string | yes | `"read"` or `"read_write"` |

Resource sandboxing restricts builtin tool access. Path traversal (`..`) is denied. If no resources are defined, all paths are allowed.

### `[pricing]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `input_per_million` | f64 | yes | Cost per million input tokens (USD, non-negative) |
| `output_per_million` | f64 | yes | Cost per million output tokens (USD, non-negative) |

Optional. Overrides the builtin model registry pricing. Cost is reported in the `done` event.

## Context File

Resume a conversation by passing `--context` with a JSON file:

```json
{
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "hello"}]},
    {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}
  ]
}
```

Content blocks support types: `text`, `tool_use`, `tool_result`, `thinking`.

## Provider Support

| API Type | Providers |
|----------|-----------|
| **Messages API** (native) | Anthropic (Claude) |
| **Chat Completions** | OpenAI, OpenRouter, Groq, Mistral, Ollama, DeepSeek, etc. |

### Provider Examples

Anthropic:

```toml
[provider.anthropic]
api = "messages"
```

OpenAI:

```toml
[provider.openai]
api = "chat_completions"
base_url = "https://api.openai.com"
```

OpenRouter:

```toml
[provider.openrouter]
api = "chat_completions"
base_url = "https://openrouter.ai/api"
[provider.openrouter.compat]
explicit_tool_choice_auto = true
```

Ollama (local):

```toml
[provider.ollama]
api = "chat_completions"
base_url = "http://localhost:11434"
[provider.ollama.compat]
skip_stream_options = true
```

## Credential Store

Credentials are stored at `~/.flick/credentials` (TOML, encrypted with ChaCha20-Poly1305). A 256-bit secret key is generated on first use and stored at `~/.flick/.secret_key` with restrictive file permissions.

```sh
# Store a credential
flick setup anthropic

# Credentials are referenced by name in config
[provider.anthropic]
credential = "anthropic"   # matches the name passed to `flick setup`
```

## HTTP Retry

The initial HTTP request uses exponential backoff for transient errors:

- **Retryable:** 429 (rate limit), 5xx (server error), network errors
- **Non-retryable:** 401 (auth), other 4xx (client error)
- **Defaults:** 3 retries, 500ms initial delay, 2x multiplier, 30s cap
- **429 responses:** `Retry-After` header overrides computed backoff

Retry applies only to the initial request. Once SSE streaming begins, no retries are attempted (events have already been emitted to stdout).

## Testing

```sh
cargo test
```

295 tests (263 lib, 4 bin, 16 agent, 12 integration). One additional Unix-only test for file permissions.

## License

MIT — see [LICENSE](LICENSE) for details.
