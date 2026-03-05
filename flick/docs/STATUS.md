# Flick — Status

## Current State

Workspace structure: `flick` library crate + `flick-cli` binary crate. Monadic architecture — single model call per invocation, JSON result to stdout, caller drives the agent loop. Two-step structured output for Chat Completions providers (tools + output_schema) implemented in the runner.

Library extraction complete (Phases 1–2 of `docs/LIBRARY_EXTRACTION.md`):
- Workspace with `flick/` (library) and `flick-cli/` (binary)
- `FlickClient` public API: `new(config, provider)`, `run()`, `resume()`, `build_request()`, `config()`
- `resolve_provider()` standalone function for credential resolution
- `Config::from_str()` with `ConfigFormat` enum for non-file config sources
- Provider always injected into `FlickClient` — fully testable with mocks
- `DynProvider` implemented directly on providers (no intermediate `Provider` trait)
- `TerminalPrompter` and `dialoguer` moved from library to CLI crate
- Shared test doubles in `flick/src/test_support.rs` behind `testing` feature
- `clap` gated behind optional `cli` feature flag
- 305 tests passing (222 lib, 54 bin, 18 runner, 11 integration), zero clippy errors

## Next Work

- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
- Fix Later items (see `BACKLOG.md`)
