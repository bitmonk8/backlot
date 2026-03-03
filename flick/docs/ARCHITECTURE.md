# Flick — Architecture

## Module Structure

```
src/
  lib.rs               Library entry point, re-exports all public modules
  main.rs              CLI parsing (clap), dispatch to run/setup
  config.rs            TOML config parsing, Config struct
  context.rs           Context, Message, ContentBlock, Role
  credential.rs        Encrypted credential store (ChaCha20-Poly1305)
  error.rs             FlickError, ProviderError, CredentialError, ToolError, ConfigError
  event.rs             StreamEvent enum, Usage, EventEmitter trait, JsonLines/Raw emitters
  history.rs           Run history logging and content-addressable context storage (xxh3-128)
  model.rs             ModelInfo, builtin registry, reasoning level mappings
  provider.rs          Provider trait, DynProvider, RequestParams, ToolDefinition, create_provider()
  provider/
    messages.rs        Messages API (Anthropic), response parsing
    chat_completions.rs  Chat Completions API, response parsing
    http.rs            HTTP retry with exponential backoff
  tool.rs              ToolRegistry, builtin tools, custom tool execution, resource sandboxing
  agent.rs             Agent loop (query → tools → repeat)
```

## Data Flow

```
CLI args
  → Config::load() + CredentialStore::get()
  → create_provider() → Box<dyn DynProvider>
  → ToolRegistry::from_config()
  → Context (from --context file or empty)
  → agent::run()
      ├─ provider.call_boxed(params) → ModelResponse
      ├─ emit events to stdout via EventEmitter
      ├─ ToolRegistry::execute() for each tool call
      └─ loop until no tool calls or iteration limit
```

## Provider Abstraction

`Provider` trait with two methods:
- `call()` — returns `Result<ModelResponse, ProviderError>` (complete response)
- `build_request()` — returns request body as JSON (for `--dry-run`)

`DynProvider` is the object-safe wrapper (`call_boxed()` adapts the async trait method for object safety). `create_provider()` dispatches by `ApiKind`.

Provider quirks are handled by `CompatFlags` (boolean fields in config), not by subclassing.

## Credential Store

- Location: `~/.flick/`
- `.secret_key` — 256-bit random key (hex-encoded), restrictive permissions
- `credentials` — TOML file with `enc3:hex(nonce||ciphertext||tag)` values
- Encryption: ChaCha20-Poly1305 AEAD

## HTTP Retry

Both providers use `http::send_with_retry()` for HTTP requests. Retryable errors (429, 5xx, network errors) trigger exponential backoff. Non-retryable errors (401, 4xx client errors, response parse errors) fail immediately. The `Retry-After` header from 429 responses overrides the computed backoff. Defaults: 3 retries, 500ms initial delay, 2x multiplier, 30s cap.

## Tool Execution

Four builtin tools: `read_file`, `write_file`, `list_directory`, `shell_exec`. All gated by config flags and resource access lists.

Custom tools support two modes:
- `command` — shell command template with `{{param}}` substitution
- `executable` — receives JSON on stdin, returns output on stdout

## Reasoning Levels

Abstract levels mapped per-provider:

| Level | Anthropic (budget_tokens) | OpenAI (reasoning_effort) |
|-------|--------------------------|--------------------------|
| minimal | 1024 | "low" |
| low | 4096 | "low" |
| medium | 10000 | "medium" |
| high | 32000 | "high" |
