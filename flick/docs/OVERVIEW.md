# Flick — Overview

## What is Flick?

Flick is an ultra-small, ultra-fast command-line tool written in Rust. It replaces ZeroClaw as the agent primitive for [Epic](../../../epic/).

Flick takes a TOML config and a query, sends the query to an LLM, emits typed events to stdout, and executes tools internally via an agent loop.

## Relationship to Epic

Epic invokes Flick as a subprocess. Flick handles a single agent session (query → model → tools → response). Epic handles orchestration, task decomposition, and state management.

| Project | Role |
|---------|------|
| Epic | Orchestrator — recursive task decomposition, state management, TUI |
| Flick | Agent primitive — LLM call with tools, JSON-lines output, agent loop |

## CLI Interface

```
flick run --config <toml> [--query <text>] [--context <json>] [--raw] [--dry-run] [--model <id>] [--reasoning <level>]
flick setup <provider>
```

- `run`: query the model, stream events to stdout
- `setup`: interactive credential onboarding per provider
- Query from `--query` or stdin
- `--context`: JSON file with prior message history
- `--raw`: plain text output instead of JSON-lines
- `--dry-run`: dump API request as JSON, no model call

## Output Format (JSON-lines, default)

```json
{"type":"text","text":"Hello, world!"}
{"type":"thinking","text":"..."}
{"type":"thinking_signature","signature":"sig_..."}
{"type":"tool_call","call_id":"tc_1","tool_name":"read_file","arguments":"{...}"}
{"type":"tool_result","call_id":"tc_1","success":true,"output":"..."}
{"type":"usage","input_tokens":1200,"output_tokens":340,"cache_creation_input_tokens":800,"cache_read_input_tokens":400}
{"type":"done","usage":{"input_tokens":1200,"output_tokens":340,"cost_usd":0.0087,"iterations":2}}
{"type":"error","message":"...","code":"rate_limit","fatal":true}
```

The `usage` event's `cache_creation_input_tokens` and `cache_read_input_tokens` fields are omitted when zero.

## Agent Loop

1. Call provider with message history
2. Emit response events to stdout (text, thinking, tool calls, usage)
3. Append assistant message to history
4. If no tool calls → emit `done`, exit
5. Execute tool calls, emit `tool_result` events
6. Append tool results to history
7. Goto 1 (cap at 25 iterations)

## Design Principles

- **Ultra-small.** Minimal binary, minimal dependencies (14 runtime crates (+1 Windows-only)).
- **Ultra-fast.** Negligible startup overhead. Time-to-first-token is the bottleneck.
- **Unix-philosophy.** Takes input, produces output, composes via stdin/stdout.
- **No framework.** Single executable, not an SDK or library.
- **Tool-calling models only.** No capability-checking fallbacks.
- **Compatibility-by-configuration.** Provider quirks via flags, not subclasses.

## Provider Support

| API Type | Providers |
|----------|-----------|
| **Messages API** (native) | Anthropic (Claude) |
| **Chat Completions** | OpenAI, OpenRouter, Groq, Mistral, Ollama, DeepSeek, etc. |

## Language & Toolchain

- **Language:** Rust
- **Edition:** 2024
- **Minimum Rust version:** 1.85

## Document Index

| Document | Purpose |
|----------|---------|
| [OVERVIEW.md](OVERVIEW.md) | This file — project context and design |
| [STATUS.md](STATUS.md) | Current phase, milestones, blockers |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Module descriptions and data flow |
| [CONFIGURATION.md](CONFIGURATION.md) | Full config reference |
| [SANDBOX.md](SANDBOX.md) | Sandboxing design (wrapper prefix, native, containers) |
