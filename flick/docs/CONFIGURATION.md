# Flick — Configuration Reference

Flick is configured via a TOML file passed with `--config`.

## Full Example

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
credential = "anthropic"

[provider.openrouter]
api = "chat_completions"
credential = "openrouter"
[provider.openrouter.compat]
explicit_tool_choice_auto = true

[[tools]]
name = "read_file"
description = "Read a file's contents"
parameters = { type = "object", properties = { path = { type = "string" } }, required = ["path"] }

[[tools]]
name = "search_codebase"
description = "Search files for a pattern"
parameters = { type = "object", properties = { pattern = { type = "string" } }, required = ["pattern"] }

[pricing]
input_per_million = 3.0
output_per_million = 15.0
```

## Sections

### `[model]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `provider` | string | yes | — | Provider name (must match a key in `[provider.*]`) |
| `name` | string | yes | — | Model identifier |
| `max_tokens` | u32 | no | 8192 | Maximum output tokens (must be > 0) |
| `temperature` | f32 | no | none | Sampling temperature (0.0–1.0 for Messages API, 0.0–2.0 for Chat Completions API) |

### `[model.reasoning]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `level` | string | yes | One of: `minimal`, `low`, `medium`, `high` |

### `system_prompt`

Top-level string. Optional system prompt sent to the model.

### `[output_schema]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema` | JSON value | yes | JSON Schema for structured output |

### `[provider.<name>]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `api` | string | yes | — | `"messages"` or `"chat_completions"` |
| `credential` | string | no | provider name | Key name in credential store |

### `[provider.<name>.compat]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `explicit_tool_choice_auto` | bool | false | Send `tool_choice: "auto"` explicitly |

### `[[tools]]`

Tool definitions declared to the model. Flick includes these in the model request but never executes tools — the caller handles execution externally.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Tool name sent to model (must be unique, non-empty) |
| `description` | string | yes | Tool description sent to model |
| `parameters` | JSON value | no | JSON Schema for tool parameters |

All tools are uniform: a name, a description, and an optional JSON schema.

### `[pricing]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `input_per_million` | f64 | yes | Cost per million input tokens, non-negative finite (USD) |
| `output_per_million` | f64 | yes | Cost per million output tokens, non-negative finite (USD) |

Optional. Overrides the builtin model registry pricing. If omitted, pricing is looked up by model name.

## Context Resumption (`--resume`)

Prior context is loaded by hash from `~/.flick/contexts/{hash}.json`. Use `--resume <hash>` with `--tool-results <file>` to continue a session after executing tool calls.

Tool results file format:
```json
[
  {"tool_use_id": "tc_1", "content": "file contents here", "is_error": false},
  {"tool_use_id": "tc_2", "content": "command not found", "is_error": true}
]
```

## Credential Store

Credentials are stored at `~/.flick/credentials` (TOML, encrypted). Use `flick setup <provider>` to add credentials interactively.
