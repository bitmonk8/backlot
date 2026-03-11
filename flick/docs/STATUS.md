# Flick — Status

## Current State

Cargo workspace: `flick` library crate + `flick-cli` binary crate.

- Monadic architecture — single model call per invocation, JSON result to stdout, caller drives the agent loop
- Two provider types: Messages API (Anthropic) and Chat Completions (OpenAI-compatible)
- Two-step structured output for Chat Completions providers (tools + output_schema)
- **Named models** — five-type decomposition: `ProviderRegistry`, `ProviderInfo`, `ModelRegistry`, `ModelInfo`, `RequestConfig`
- `ProviderRegistry` at `~/.flick/providers` (TOML, encrypted keys, compat flags)
- `ModelRegistry` at `~/.flick/models` (TOML, user-defined model aliases with provider ref, model ID, max_tokens, pricing)
- `RequestConfig` (formerly `Config`) — model string key, system_prompt, temperature, reasoning, output_schema, tools
- `RequestConfig::builder()` for programmatic construction
- `FlickClient::new(request, &models, &providers)` — resolves model→provider chain at construction
- `validate_registries()` cross-registry validation (ModelInfo.provider → ProviderRegistry key)
- `FlickClient::with_provider()` for test injection (mock providers)
- `DynProvider` implemented directly on providers (no intermediate `Provider` trait)
- Interactive prompts (`TerminalPrompter`, `dialoguer`) in CLI crate only
- Shared test doubles in `flick/src/test_support.rs` behind `testing` feature
- `clap` gated behind optional `cli` feature flag
- Config validation: `deny_unknown_fields` on all structs, reasoning+output_schema mutual exclusion, empty tool description/non-object parameters rejected, whitespace-only query early rejection
- CLI commands: `provider add/list`, `model add/list/remove`, `init`, `run`
- 250 tests passing (199 lib, 22 bin, 18 runner, 11 integration), zero clippy errors

## Next Work

- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
- Backlog items (see `BACKLOG.md` — 31 items in 7 active clusters)
