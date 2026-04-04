# Flick — Status

## Current State

Cargo workspace: `flick` library crate + `flick-cli` binary crate.

- Monadic architecture — single model call per invocation, JSON result to stdout, caller drives the agent loop
- Two provider types: Messages API (Anthropic) and Chat Completions (OpenAI-compatible)
- Two-step structured output for Chat Completions providers (tools + output_schema), with context restoration on second-call failure
- **Named models** — five-type decomposition: `ProviderRegistry`, `ProviderInfo`, `ModelRegistry`, `ModelInfo`, `RequestConfig`
- `ProviderRegistry` at `~/.flick/providers` (TOML, encrypted keys, compat flags, name/URL validation with `url::Url::parse`, fsync'd key writes, atomic file operations with cleanup)
- `ModelRegistry` at `~/.flick/models` (TOML, user-defined model aliases with provider ref, model ID, max_tokens, pricing including cache creation/read tiers)
- `RequestConfig` — model string key, system_prompt, temperature, reasoning, output_schema, tool_choice, tools
- `RequestConfig::builder()` for programmatic construction
- `FlickClient::new(request, &models, &providers)` — resolves model->provider chain at construction
- `validate_registries()` cross-registry validation (ModelInfo.provider -> ProviderRegistry key), tested
- `FlickClient::new_with_provider()` for test injection (mock providers)
- `DynProvider` implemented directly on providers (no intermediate `Provider` trait)
- Interactive prompts (`TerminalPrompter`, `dialoguer`) in CLI crate only
- Shared test doubles in `flick/src/test_support.rs` behind `testing` feature
- `clap` gated behind optional `cli` feature flag
- Config validation: `deny_unknown_fields` on all structs, reasoning+output_schema mutual exclusion, temperature+thinking mutual exclusion (enforced at both config and provider level), empty tool description/non-object input_schema rejected, whitespace-only query early rejection, `validate_resolved` in `validation.rs` module
- CLI input hardening — stdin capped at 10 MiB, provider name length validated (max 255), API key content validated (no control chars, max 4096), whitespace-only stdin produces distinct error
- CLI commands: `provider add/list`, `model add/list/remove`, `init`, `run`
- Cache-aware cost computation — `compute_cost` on `ModelInfo`, plain arithmetic for readability, both providers normalize `input_tokens` to non-cached tokens (total minus cache_creation and cache_read) for consistent cross-provider semantics
- Context serialization robustness — custom `ContentBlock` deserializer (direct field extraction, no inner enum), message ordering validation on load (including `ToolUse`-in-user check), empty-content assistant validation on load, `push_*` methods enforce message alternation and reject pushes on empty context, `check_capacity` helper for overflow detection, serde defaults for optional fields
- Error type hygiene — `CredentialError` split into specific variants (`InvalidProviderName`, `InvalidBaseUrl`, `InvalidSecretKey`, `TomlParse`), `ProviderError::InvalidRequest` for client-side validation, `ProviderError::code()` delegation, explicit `serde_json::Error` mapping (no blanket `From`)
- Messages API: system prompt serialized as content-block array, `tool_choice` support (`auto`/`any`/`none`/`tool`)
- Prompt caching — 2-breakpoint strategy: `cache_control` injected on system text block (BP1, caches tools+system prefix) and last user message block (BP2, caches growing conversation history). `CacheRetention` enum (`None`/`Short`/`Long`) configurable per-request via builder or setter; `Short` (ephemeral, 5-min TTL) is the default. `Long` adds `"ttl": "1h"` for extended caching. Safe to inject unconditionally across all providers (ignored by OpenAI/DeepSeek, passed through by OpenRouter/LiteLLM)
- Chat Completions: `tool_choice` support mapped to equivalent values (`auto`/`required`/`none`/function spec)
- `ToolChoice` enum in provider layer, `ToolChoiceConfig` in config layer with serde support and validation
- Module organization: `crypto.rs` (encrypt/decrypt), `platform.rs` (Windows permissions), `validation.rs` (resolved config validation)
- `CompatFlags` in `provider_registry.rs` (describes provider behavior), `ToolConfig::input_schema` aligned with `ToolDefinition::input_schema` (backward compat via `#[serde(alias = "parameters")]`)
- Secret key file write logic extracted to `write_new_secret_key_file` helper (shared across Unix/Windows)
- Per-call timing — `FlickResult.timing` contains `api_latency_ms` measured around provider calls (summed for two-step structured output)
- Structured output cleaning — `structured_output.rs` provides `strip_fences_from_blocks` (fence stripping) and `check_required_fields` (required-field validation, recursive including array items); `FlickError::ResponseNotJson` for unparseable JSON, `FlickError::SchemaValidation` for schema conformance failures; inline context rollback (pop stale assistant, restore prior message) in both `run` and `run_second_step`
- 373 tests passing (316 lib, 26 bin, 20 runner, 11 integration), zero clippy errors

## Next Work

- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
