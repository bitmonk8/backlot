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
| `base_url` | string | no | per-API default | API base URL |
| `credential` | string | no | provider name | Key name in credential store |

### `[provider.<name>.compat]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `explicit_tool_choice_auto` | bool | false | Send `tool_choice: "auto"` explicitly |
| `skip_stream_options` | bool | false | Omit `stream_options` from request (for providers that reject it) |

### `[tools]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `read_file` | bool | false | Enable read_file builtin |
| `write_file` | bool | false | Enable write_file builtin |
| `list_directory` | bool | false | Enable list_directory builtin |
| `shell_exec` | bool | false | Enable shell_exec builtin |

> **Security: `shell_exec` bypasses resource restrictions.** When `shell_exec = true`, the model can execute arbitrary shell commands with no resource access validation. The `[[resources]]` sandbox applies only to `read_file`, `write_file`, and `list_directory`. A model can use `shell_exec` to read, write, or delete any file the process user can access, regardless of configured resources. Treat `shell_exec = true` as granting the model unrestricted system access.

### `[[tools.custom]]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Tool name sent to model |
| `description` | string | yes | Tool description sent to model |
| `parameters` | JSON value | no | JSON Schema for tool parameters |
| `command` | string | no* | Shell command template (`{{param}}` substitution) |
| `executable` | string | no* | Path to executable (receives JSON on stdin) |

*Exactly one of `command` or `executable` is required (not both). Tool names must be unique and cannot collide with builtin tool names.

> **Security: command-mode custom tools bypass resource restrictions.** Custom tools using `command` execute shell commands with model-supplied parameter values substituted into the template. These commands run outside the `[[resources]]` sandbox. Model-controlled parameters can reference arbitrary paths. Treat command-mode custom tools as having the same unrestricted access as `shell_exec`.

### `[[resources]]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `path` | string | yes | Path to file or directory |
| `access` | string | yes | `"read"` or `"read_write"` |

If no resources are defined, all paths are allowed.

### `[pricing]`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `input_per_million` | f64 | yes | Cost per million input tokens, non-negative finite (USD) |
| `output_per_million` | f64 | yes | Cost per million output tokens, non-negative finite (USD) |

Optional. Overrides the builtin model registry pricing. If omitted, pricing is looked up by model name.

## Context File (`--context`)

JSON file with prior message history:

```json
{"messages": [
  {"role": "user", "content": [{"type": "text", "text": "hello"}]},
  {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}
]}
```

## Credential Store

Credentials are stored at `~/.flick/credentials` (TOML, encrypted). Use `flick setup <provider>` to add credentials interactively.
