# Flick — Status

## Current State

Monadic architecture implemented. Flick makes a single model call per invocation and returns a JSON result. The caller drives the agent loop. 274 tests pass (206 lib, 48 bin, 12 runner, 8 integration).

## Module Summary

| Module | Description |
|--------|-------------|
| `main.rs` | CLI parsing, dispatch, `flick init` interactive config generator, JSON result output |
| `config.rs` | TOML config parsing with validation (`[[tools]]` array for tool definitions) |
| `context.rs` | Message history types (with thinking signature), tool result loading |
| `credential.rs` | Encrypted credential store (corruption-safe, restricted permissions) |
| `error.rs` | Error types (all thiserror-derived) |
| `result.rs` | `FlickResult` struct — single JSON output type (status, content, usage, context_hash, error) |
| `model.rs` | Model registry + reasoning levels |
| `provider.rs` | Provider trait + factory |
| `provider/http.rs` | HTTP retry with exponential backoff |
| `provider/messages.rs` | Messages API (Anthropic), non-streaming response parsing |
| `provider/chat_completions.rs` | Chat Completions API, non-streaming response parsing |
| `runner.rs` | Single model call, returns `FlickResult` (public `build_params` for dry-run) |
| `history.rs` | Run history logging and content-addressable context storage (xxh3-128) |
| `model_list.rs` | Model fetching from provider APIs (HttpModelFetcher, MockModelFetcher) |
| `prompter.rs` | Prompter trait + TerminalPrompter (dialoguer) + MockPrompter (tests) |

## Next Work

- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
- Fix Later items (see `REVIEW_FINDINGS.md`)

## Decisions

| Decision | Rationale |
|----------|-----------|
| Monadic / single-shot architecture | Flick makes one model call per invocation and returns. Caller drives the agent loop. |
| Single JSON result output | One invocation = one JSON object. |
| `--resume` + `--tool-results` | Caller resumes sessions by context hash and supplies tool results as a JSON file. |
| Tool definitions only | `[[tools]]` declares name, description, parameters. |
| Rust, edition 2024 | Consistent with Epic; same toolchain |
| CLI tool, not library | Unix-philosophy: single executable, composable |
| Messages API first-class, Chat Completions for breadth | Two provider implementations cover all targets |
| Tool-calling models only | No fallback for models without native function calling |
| Compat flags over subclasses | Provider quirks via configuration |
| ChaCha20-Poly1305 credential encryption | Same scheme as ZeroClaw, proven |
| 15 crate dependencies (+1 Windows-only) | Minimal footprint; no anyhow, async_trait, tracing |
| DynProvider trait | Required for object-safe async dispatch |
| `toml` crate (not `basic-toml`) | `basic-toml` has compiler bug with edition 2024 |
| `expect_used = "deny"` lint | Prevents `.expect()` in production code; test modules `#[allow]` |
| reqwest 0.12 (not 0.13) | 0.13 blocked by rustc 1.93.1 ICE on `windows-sys` 0.61.2 |
| Non-streaming API calls | Simpler error handling, full response before output |
