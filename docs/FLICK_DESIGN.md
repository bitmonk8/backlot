# Design

## Five Core Types

| Type | Responsibility | Storage |
|---|---|---|
| `ProviderRegistry` | Map of name -> `ProviderInfo` | `~/.flick/providers` (TOML) |
| `ProviderInfo` | API type, base URL, encrypted credential, compat flags | Entry in ProviderRegistry |
| `ModelRegistry` | Map of name -> `ModelInfo` | `~/.flick/models` (TOML) |
| `ModelInfo` | Provider ref, model ID, max_tokens, pricing | Entry in ModelRegistry |
| `RequestConfig` | Model ref, system_prompt, tools, output_schema, temperature, reasoning | Per-invocation YAML/JSON file |

## Resolution Chain

```
RequestConfig.model ("balanced")
    -> ModelRegistry["balanced"] -> ModelInfo { provider: "anthropic", name: "claude-sonnet-4-6", ... }
        -> ProviderRegistry["anthropic"] -> ProviderInfo { api: messages, base_url: "https://api.anthropic.com", ... }
```

Resolution happens once at `FlickClient::new()`. Errors (unknown model name, unknown provider) fail at construction, not at call time.

## Data Flow

**New session** (`--query`):
```
CLI args
  -> RequestConfig::load() + ProviderRegistry::load_default() + ModelRegistry::load_default()
  -> validate_registries(&models, &providers)
  -> FlickClient::new(request, &models, &providers)  [resolves model -> provider chain]
  -> Context (empty) + user query
  -> runner::run()  [single model call]
      +-- config.tools() -> Vec<ToolDefinition>
      +-- provider.call_boxed(params) -> ModelResponse
      +-- append assistant message to context
      +-- return FlickResult (status: complete | tool_calls_pending)
  -> write context file, set context_hash
  -> serialize FlickResult as JSON to stdout
```

**Resume session** (`--resume <hash>` + `--tool-results <file>`):
```
CLI args
  -> RequestConfig::load() + ProviderRegistry::load_default() + ModelRegistry::load_default()
  -> validate_registries(&models, &providers)
  -> FlickClient::new(request, &models, &providers)
  -> Context (loaded from ~/.flick/contexts/{hash}.json)
  -> load tool results from --tool-results file
  -> append tool results as user message to context
  -> runner::run()  [single model call]
  -> write context file, set context_hash
  -> serialize FlickResult as JSON to stdout
```

## Provider Abstraction

Two provider implementations:
- **Messages** (`messages.rs`) — Anthropic native API
- **ChatCompletions** (`chat_completions.rs`) — OpenAI-compatible API

`DynProvider` is the object-safe wrapper (`call_boxed()` adapts the async trait method for object safety). `FlickClient::new()` builds the appropriate provider from the resolved `ProviderInfo`.

Provider quirks are handled by `CompatFlags` (boolean fields in `ProviderInfo`), not by subclassing.

## Design Rationale

- `model` in RequestConfig is always a string key into ModelRegistry — no inline model definitions.
- TOML for both registries.
- No builtin models — ModelRegistry is purely user-defined.
- No CLI override flags — the RequestConfig file is the sole source of request parameters.
- Builder pattern for programmatic RequestConfig construction (library consumers vary configs per call).
- `validate_registries()` checks cross-registry reference integrity after both registries are loaded. `FlickClient::new()` assumes it already ran.
- `flick init` generates a RequestConfig file only. Directs user to `flick model add` / `flick provider add` if registries are empty.

## Validation

Three layers, each with a distinct scope:

**ModelRegistry** (on load):
- Non-empty `name` and `provider`
- `max_tokens` > 0 if present
- Pricing fields non-negative and finite

**Cross-registry** (`validate_registries`, called once after both registries are loaded):
- Every `ModelInfo.provider` must reference an existing key in the ProviderRegistry

**RequestConfig** (at `FlickClient::new()`):
- `model` references a key in ModelRegistry
- `temperature` non-negative, finite, and within API-specific ceiling (1.0 Messages, 2.0 ChatCompletions)
- `reasoning` + `output_schema` mutual exclusion (Messages API)
- `budget_tokens` < `max_tokens` (Messages API with reasoning)
- Tool names non-empty and unique
- Tool descriptions non-empty
- Tool parameters are JSON objects if present

## Library / CLI Boundary

The `flick` library crate and `flick-cli` binary crate have a strict separation:

1. **Library must not start a tokio runtime.** All async methods assume the caller provides one. The CLI crate owns `#[tokio::main]`.
2. **Library must not write to stdout/stderr.** All output is via return values. The CLI crate handles printing.
3. **Library must not call `std::process::exit`.** Errors are returned, not fatal.
4. **Context persistence is opt-in.** `FlickClient::run()` returns a `FlickResult` containing the updated `Context`. The caller decides whether to persist it. The CLI writes context files; library users may keep context in memory.
5. **History recording is opt-in.** The `history` module is public but not called automatically. The CLI calls it; library users may skip it.
6. **Interactive prompts live in the CLI.** `TerminalPrompter` and `dialoguer` are CLI-only dependencies.
