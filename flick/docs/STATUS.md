# Flick — Status

## Current State

Cargo workspace: `flick` library crate + `flick-cli` binary crate.

- Monadic architecture — single model call per invocation, JSON result to stdout, caller drives the agent loop
- Two provider types: Messages API (Anthropic) and Chat Completions (OpenAI-compatible)
- Two-step structured output for Chat Completions providers (tools + output_schema)
- `FlickClient` public API: `new(config, provider)`, `run()`, `resume()`, `build_request()`, `config()`
- `resolve_provider()` standalone function for credential resolution
- `Config::from_str()` with `ConfigFormat` enum for non-file config sources
- Provider injected into `FlickClient` — fully testable with mocks
- `DynProvider` implemented directly on providers (no intermediate `Provider` trait)
- Interactive prompts (`TerminalPrompter`, `dialoguer`) in CLI crate only
- Shared test doubles in `flick/src/test_support.rs` behind `testing` feature
- `clap` gated behind optional `cli` feature flag
- Config validation: `deny_unknown_fields` on all structs, reasoning+output_schema mutual exclusion, empty tool description/non-object parameters rejected, whitespace-only query early rejection, unknown-model pricing warning
- 333 tests passing (248 lib, 56 bin, 18 runner, 11 integration), zero clippy errors

## Next Work

- **Named models** — implement `docs/NAMED_MODELS.md` (model alias registry with builtin + user-defined names)
- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
- Backlog items (see `BACKLOG.md` — 31 items in 7 active clusters)
