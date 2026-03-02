# Flick — Status

## Current State

Implementation complete. 315 tests pass (276 lib, 10 bin, 17 agent, 12 integration).

## Module Summary

| Module | Description |
|--------|-------------|
| `main.rs` | CLI parsing, dispatch |
| `config.rs` | TOML config parsing with validation (private fields, getters, validated CLI overrides) |
| `context.rs` | Message history types (with thinking signature) |
| `credential.rs` | Encrypted credential store (corruption-safe, restricted permissions) |
| `error.rs` | Error types (all thiserror-derived) |
| `event.rs` | Stream event types + emitters (buffered) |
| `model.rs` | Model registry + reasoning levels |
| `provider.rs` | Provider trait + factory |
| `provider/sse.rs` | Shared SSE parsing infrastructure (SmallVec), HTTP retry with exponential backoff |
| `provider/messages.rs` | Messages API SSE streaming (per-block tracking, message_stop) |
| `provider/chat_completions.rs` | Chat Completions SSE streaming (BTreeMap, stream_options, is_error, null content) |
| `tool.rs` | Builtin + custom tool execution (shell-escape, timeout, sandbox) |
| `agent.rs` | Agent loop (public build_params) |

## Next Work

- Clippy 1.93 fix: `unnecessary_debug_formatting` in `tool.rs:164`
- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
- Fix Later items (26 open, see `REVIEW_FINDINGS.md`)

## Decisions

| Decision | Rationale |
|----------|-----------|
| Rust, edition 2024 | Consistent with Epic; same toolchain |
| CLI tool, not library | Unix-philosophy: single executable, composable |
| Messages API first-class, Chat Completions for breadth | Two provider implementations cover all targets |
| Tool-calling models only | No fallback for models without native function calling |
| Compat flags over subclasses | Provider quirks via configuration |
| ChaCha20-Poly1305 credential encryption | Same scheme as ZeroClaw, proven |
| JSON-lines streaming default | Machine-readable, one event per line |
| 14 crate dependencies (+1 Windows-only) | Minimal footprint; no anyhow, async_trait, tracing |
| DynProvider trait | Required for object-safe async dispatch |
| `toml` crate (not `basic-toml`) | `basic-toml` has compiler bug with edition 2024 |
| `expect_used = "deny"` lint | Prevents `.expect()` in production code; test modules `#[allow]` |
| reqwest 0.12 (not 0.13) | 0.13 blocked by rustc 1.93.1 ICE on `windows-sys` 0.61.2 |
| Retry only pre-stream | Mid-stream retry not viable — SSE events already emitted to stdout |
