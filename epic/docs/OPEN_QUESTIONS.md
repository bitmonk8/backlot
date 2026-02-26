# Open Questions

Unresolved design decisions requiring investigation or user input.

## ZeroClaw Integration

- [x] **Integration mode**: Library import via wrapper binary. Epic depends on ZeroClaw as a Rust crate and uses `AgentBuilder` API to construct agents with custom `RuntimeAdapter`, custom tools, and per-call system prompts. Each agent call builds a new `Agent`, sends one prompt via `run_single()`, and reads the response + history. Requires two upstream PRs (see below). Alternative runtime under evaluation.
- [x] **ZeroClaw maturity**: v0.1.7, early but functional. Key gaps: `SecurityPolicy` is `pub(crate)` (blocks external tool construction), no Anthropic streaming, no native MCP, shell hardcoded to `sh -c` (breaks Windows). Gaps addressable via upstream PRs. Perplexity claims about "stable ABI plugin system" and "MCP support added Feb 2026" are fabricated — verified against source.
- [x] **Memory system overlap**: ZeroClaw's memory system (SQLite + optional vector backends) is independent of Epic's DocumentStore. For v1, ZeroClaw memory disabled via `NoneMemory`. DocumentStore remains Epic's own implementation. Future integration possible but not required.
- [x] **Tool definition**: ZeroClaw's `Tool` trait is public (`tools/traits.rs:22`). Per-call tool scoping achieved by constructing different tool `Vec`s per `AgentBuilder` call — Epic controls exactly which tools each agent gets. No dynamic per-session allowlists needed.
- [x] **Structured output**: Custom `submit_result` tool implementing ZeroClaw's `Tool` trait. Agent is instructed via system prompt to call `submit_result` with typed JSON. Epic reads the structured data from `agent.history()` after execution. Same pattern as fds2_epic's MCP-based `submit_result`, but native Rust.
- [x] **Provider routing**: Per-call model selection via `AgentBuilder::model_name()`. Each agent call constructs a new `Agent` with the desired model. Haiku for assessment, Opus for recovery, etc. — all work.

### Upstream PRs Required

Two changes needed in ZeroClaw to fully enable the wrapper binary approach:

1. **Make `security` module public** — Change `pub(crate) mod security` to `pub mod security` in `lib.rs:66`. Without this, external crates cannot construct `SecurityPolicy`, which every built-in tool requires. General benefit: enables any library consumer to use ZeroClaw's built-in tools.
2. **Windows shell support in NativeRuntime** — Add `#[cfg(windows)]` branch to `native.rs:42` using `cmd.exe /C` (and `scheduler.rs:430`). General benefit: makes ZeroClaw's advertised Windows binary actually functional for shell commands.

## Agent SDK

- [x] **Direct Anthropic SDK vs ZeroClaw**: All agent calls go through ZeroClaw's `AgentBuilder` API. No bypass needed — even tool-less structured output calls use an `Agent` with only the `submit_result` tool.
- [x] **Streaming**: ZeroClaw's Anthropic provider does not implement streaming (`stream_chat_with_system` returns empty stream). TUI displays event-level updates (task phase transitions, completions) rather than token streaming. Acceptable for v1. Streaming can be added later via upstream PR or custom provider wrapper.
- [x] **Claude Agent SDK in Rust**: No Rust Claude Agent SDK exists. ZeroClaw provides the equivalent: agent runtime with tool execution loop, Anthropic provider with native tool use support, conversation history management. This replaces the Python Claude Agent SDK that fds2_epic uses.

## Configuration

- [x] **Config format**: TOML. Rust ecosystem standard, shallow config fits naturally, `toml` crate is mature/serde-native. YAML has archived crate and implicit type coercion footguns. RON is too niche for a project-agnostic tool.
- [x] **Init command**: Yes. `epic init` runs an agent that explores the project (build system, test framework, linters, formatters), presents findings, and interactively confirms which verification steps to enable. Writes `epic.toml` with confirmed choices; declined options commented out. Minimum output is always a valid scaffold.
- [x] **ZeroClaw config ownership**: Epic owns all config. No separate ZeroClaw config file. The `[zeroclaw]` section in `epic.toml` exposes any ZeroClaw-specific knobs. ZeroClaw is used as a library — all settings are passed programmatically via `AgentBuilder`.
- [x] **Config inheritance**: Yes. User-level defaults in `~/.config/epic/config.toml`, overridden field-by-field by project `epic.toml`. Useful for default model preferences, API key location, personal limits.

## Document Store

- [x] **File-based vs database**: File-based (markdown) for v1. Document count per run is small (tens), plain files are inspectable/diffable, librarian routes by semantic understanding not SQL queries. SQLite index can layer on later if needed.
- [x] **Librarian implementation**: ZeroClaw agent. Routed through `AgentBuilder` like all other agent calls — Haiku model, read-only tools, `submit_result` for document placement decision. Consistent with "all agent calls through ZeroClaw" pattern.

## TUI

- [x] **Framework**: ratatui with crossterm backend. De facto Rust TUI framework, async-compatible with tokio, actively maintained successor to tui-rs.
- [x] **Interaction model**: Read-only monitoring for v1. Orchestrator emits events consumed by TUI (task state changes, log entries). Decoupled architecture supports adding interactive controls later without rearchitecting.

## Rust-Specific

- [x] **Async runtime**: tokio. Ecosystem standard, ZeroClaw already uses it internally, async-std effectively dormant since 2023.
- [x] **Error handling**: anyhow + thiserror. `thiserror` for structured error enums at module boundaries, `anyhow` for propagation with context in application code.
- [x] **Serialization**: serde + serde_json + toml. The only viable path — serde is the Rust serialization framework, serde_json for agent structured output and state persistence, toml crate for config.

## Scope

- [x] **Parallel execution**: Out of scope for v1. Sequential execution must be proven robust before introducing parallelism. Consistent with EPIC_DESIGN2.
- [x] **Multi-language support**: No special handling for v1. Configurable verification commands already make Epic language-agnostic. Agents read/write files regardless of language. Language-aware features (AST parsing, smarter context selection) can be added incrementally later.
- [x] **Git hosting integration**: Out of scope for v1. Git operations (commit, branch) happen through agent shell access. PR creation, issue linking are workflow conveniences that can layer on later.
