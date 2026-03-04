# Flick — Architecture

## Module Structure

```
src/
  lib.rs               Library entry point, re-exports all public modules, ApiKind enum
  main.rs              CLI parsing (clap), dispatch to run/setup/init, JSON result output
  config.rs            TOML config parsing, Config struct, ToolConfig (declaration-only)
  context.rs           Context, Message, ContentBlock, Role, tool result loading
  credential.rs        Encrypted credential store (ChaCha20-Poly1305)
  error.rs             FlickError, ProviderError, CredentialError, ConfigError
  result.rs            FlickResult, ResultStatus, UsageSummary, ResultError
  history.rs           Run history logging and content-addressable context storage (xxh3-128)
  model.rs             ModelInfo, builtin registry, reasoning level mappings
  model_list.rs        Model fetching from provider APIs (HttpModelFetcher, MockModelFetcher)
  prompter.rs          Prompter trait + TerminalPrompter (dialoguer) + MockPrompter (tests)
  provider.rs          Provider trait, DynProvider, RequestParams, ToolDefinition, create_provider()
  provider/
    messages.rs        Messages API (Anthropic), response parsing
    chat_completions.rs  Chat Completions API, response parsing
    http.rs            HTTP retry with exponential backoff
  runner.rs            Single model call, returns FlickResult
```

## Data Flow

**New session** (`--query`):
```
CLI args
  → Config::load() + CredentialStore::get()
  → create_provider() → Box<dyn DynProvider>
  → Context (empty) + user query
  → runner::run()  [single model call]
      ├─ config.tools() → Vec<ToolDefinition>
      ├─ provider.call_boxed(params) → ModelResponse
      ├─ append assistant message to context
      └─ return FlickResult (status: complete | tool_calls_pending)
  → write context file, set context_hash
  → serialize FlickResult as JSON to stdout
```

**Resume session** (`--resume <hash>` + `--tool-results <file>`):
```
CLI args
  → Config::load() + CredentialStore::get()
  → create_provider() → Box<dyn DynProvider>
  → Context (loaded from ~/.flick/contexts/{hash}.json)
  → load tool results from --tool-results file
  → append tool results as user message to context
  → runner::run()  [single model call]
  → write context file, set context_hash
  → serialize FlickResult as JSON to stdout
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

## Tool Declarations

Tools are declared in config as `[[tools]]` entries with name, description, and JSON schema parameters. Flick includes these definitions in the model request but never executes tools. When the model returns tool-use blocks, the result status is `tool_calls_pending` and the caller handles execution externally.

## Reasoning Levels

Abstract levels mapped per-provider:

| Level | Anthropic (budget_tokens) | OpenAI (reasoning_effort) |
|-------|--------------------------|--------------------------|
| minimal | 1024 | "low" |
| low | 4096 | "low" |
| medium | 10000 | "medium" |
| high | 32000 | "high" |
