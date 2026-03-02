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
  model.rs             ModelInfo, builtin registry, reasoning level mappings
  provider.rs          Provider trait, DynProvider, RequestParams, ToolDefinition, create_provider()
  provider/
    messages.rs        Messages API (Anthropic), SSE parsing
    chat_completions.rs  Chat Completions API, SSE parsing
    sse.rs             Shared SSE parsing + HTTP retry with exponential backoff
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
      ├─ provider.stream_boxed(params) → EventStream
      ├─ emit events to stdout via EventEmitter
      ├─ accumulate tool calls
      ├─ ToolRegistry::execute() for each tool call
      └─ loop until no tool calls or iteration limit
```

## Provider Abstraction

`Provider` trait with two methods:
- `stream()` — returns `EventStream` (pinned boxed Stream of `Result<StreamEvent, ProviderError>`)
- `build_request()` — returns request body as JSON (for `--dry-run`)

`DynProvider` is the object-safe wrapper (`stream_boxed()` adapts the async trait method for object safety). `create_provider()` dispatches by `ApiKind`.

Provider quirks are handled by `CompatFlags` (boolean fields in config), not by subclassing.

## Credential Store

- Location: `~/.flick/`
- `.secret_key` — 256-bit random key (hex-encoded), restrictive permissions
- `credentials` — TOML file with `enc3:hex(nonce||ciphertext||tag)` values
- Encryption: ChaCha20-Poly1305 AEAD

## HTTP Retry

Both providers use `sse::send_with_retry()` for the initial HTTP request. Retryable errors (429, 5xx, network errors) trigger exponential backoff. Non-retryable errors (401, 4xx client errors, SSE parse errors) fail immediately. The `Retry-After` header from 429 responses overrides the computed backoff. Defaults: 3 retries, 500ms initial delay, 2x multiplier, 30s cap.

Retry applies only to the initial request/response exchange. Once a 2xx response is received and SSE parsing begins, no further retries are attempted — events have already been emitted to stdout and cannot be retracted.

## Streaming

SSE parsing happens in a spawned tokio task per provider. Events are sent through an mpsc channel and consumed as a `ReceiverStream`. The agent loop forwards events to the `EventEmitter` (JSON-lines or raw text).

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
