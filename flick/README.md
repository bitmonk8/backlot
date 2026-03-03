# Flick

Ultra-small, ultra-fast command-line LLM agent written in Rust. Takes a TOML config and a query, emits typed events to stdout, and executes tools via an agent loop.

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

## Output Format

By default, Flick emits one JSON object per line to stdout:

```jsonl
{"type":"text","text":"Hello, world!"}
{"type":"thinking","text":"..."}
{"type":"thinking_signature","signature":"sig_..."}
{"type":"tool_call","call_id":"tc_1","tool_name":"read_file","arguments":"{...}"}
{"type":"tool_result","call_id":"tc_1","success":true,"output":"..."}
{"type":"usage","input_tokens":1200,"output_tokens":340,"cache_creation_input_tokens":800,"cache_read_input_tokens":400}
{"type":"done","usage":{"input_tokens":1200,"output_tokens":340,"cost_usd":0.0087,"iterations":2}}
{"type":"error","message":"...","code":"rate_limit"}
```

The `usage` event's `cache_creation_input_tokens` and `cache_read_input_tokens` fields are omitted when zero.

With `--raw`, only text content is printed as plain text. Errors go to stderr.

## Agent Loop

1. Call provider with message history
2. Emit response events to stdout (text, thinking, tool calls, usage)
3. Append assistant message to history
4. If no tool calls, emit `done` and exit
5. Execute tool calls, emit `tool_result` events
6. Append tool results to history
7. Goto 1 (capped at 25 iterations)

## Tool Permissions and Safety

`[[resources]]` declares which paths builtin file tools (`read_file`, `write_file`, `list_directory`) may access. This is an in-process intent guardrail — it stops accidental out-of-scope access and makes the operator's declared policy visible.

It is not a security boundary:

- `shell_exec = true` gives the model full shell access as the process user. `[[resources]]` does not apply to shell commands.
- Custom `command` tools receive model-controlled arguments substituted into shell templates and are not restricted by `[[resources]]`.

The right mental model is the same as Claude Code's permission system: permissions reduce accidental overreach and express what the agent is supposed to do. They do not prevent a model from doing anything the process user can do.

**For hard isolation, run Flick inside a container or VM** with only the required paths mounted. That is the only way to enforce a genuine boundary on what the agent can access. A minimal Docker invocation:

```sh
docker run --rm -i \
  --cap-drop ALL \
  --network none \
  --read-only \
  -v "$(pwd)/workspace:/workspace" \
  my-flick-image \
  flick run --config /workspace/config.toml --query "..."
```

For a middle ground between in-process checks and full containerization, Flick supports an operator-configured **sandbox wrapper prefix**. When `[sandbox]` is set in the config, every subprocess invocation is prefixed with the wrapper command, allowing tools like bubblewrap, firejail, sandbox-exec, or Sandboxie-Plus to enforce OS-level constraints. See the `[sandbox]` configuration section below and `docs/SANDBOX.md` for details.

---

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

Restricts builtin file tool access (`read_file`, `write_file`, `list_directory`). Path traversal (`..`) is denied. If no resources are defined, all paths are allowed. Does not apply to `shell_exec` or custom `command` tools. See [Tool Permissions and Safety](#tool-permissions-and-safety).

### `[sandbox]`

Optional. When present, every tool subprocess invocation is prefixed with the wrapper command. Flick performs mechanical string expansion only — it has no knowledge of any specific sandbox tool.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `wrapper` | string[] | yes | — | Base wrapper command, prepended to every subprocess |
| `read_args` | string[] | no | `[]` | Appended once per `[[resources]]` entry with `access = "read"` |
| `read_write_args` | string[] | no | `[]` | Appended once per `[[resources]]` entry with `access = "read_write"` |
| `suffix` | string[] | no | `[]` | Appended once at the end, before the target command |
| `policy_file` | string | no | — | Path for generated policy file (requires `policy_template`) |
| `policy_template` | string | no | — | Template for policy file content (requires `policy_file`) |
| `policy_read_rule` | string | no | — | Per-resource line for read entries in policy file |
| `policy_read_write_rule` | string | no | — | Per-resource line for read_write entries in policy file |

**Placeholders:** `{cwd}` (working directory), `{path}` (resource path, in `read_args`/`read_write_args`/rules only), `{policy_file}` (generated policy path), `{pid}` (process ID, all fields).

**Command assembly:** `[wrapper] [read_args per read resource] [read_write_args per rw resource] [suffix] <original command>`

**Startup behavior:** If `wrapper[0]` is not found in PATH, Flick exits with an error. If `policy_file` and `policy_template` are both set, the policy file is written once at startup.

Example (bubblewrap on Linux):

```toml
[sandbox]
wrapper = ["bwrap", "--die-with-parent", "--new-session"]
read_args = ["--ro-bind", "{path}", "{path}"]
read_write_args = ["--bind", "{path}", "{path}"]
suffix = ["--"]
```

See `docs/SANDBOX.md` for additional platform examples (firejail, sandbox-exec, Sandboxie-Plus).

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

Retry applies only to the HTTP request/response exchange.

## Testing

```sh
cargo test
```

273 tests (238 lib, 10 bin, 13 agent, 12 integration). One additional Unix-only test for file permissions.

## License

MIT — see [LICENSE](LICENSE) for details.
