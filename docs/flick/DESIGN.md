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

## Library / CLI Boundary

The `flick` library crate and `flick-cli` binary crate have a strict separation:

1. **Library must not start a tokio runtime.** All async methods assume the caller provides one. The CLI crate owns `#[tokio::main]`.
2. **Library must not write to stdout/stderr.** All output is via return values. The CLI crate handles printing.
3. **Library must not call `std::process::exit`.** Errors are returned, not fatal.
4. **Context persistence is opt-in.** `FlickClient::run()` returns a `FlickResult` containing the updated `Context`. The caller decides whether to persist it. The CLI writes context files; library users may keep context in memory.
5. **History recording is opt-in.** The `history` module is public but not called automatically. The CLI calls it; library users may skip it.
6. **Interactive prompts live in the CLI.** `TerminalPrompter` and `dialoguer` are CLI-only dependencies.
