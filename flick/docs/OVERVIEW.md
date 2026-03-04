# Flick — Overview

## What is Flick?

Flick is an ultra-small, ultra-fast command-line tool written in Rust. It replaces ZeroClaw as the agent primitive for [Epic](../../../epic/).

Flick takes a TOML config and a query, sends the query to an LLM, and returns a single JSON result to stdout. Flick declares tool definitions to the model but never executes tools. The caller drives the agent loop externally.

## Relationship to Epic

Epic invokes Flick as a subprocess in a driver loop. Flick makes a single model call per invocation and returns. When the model requests tool calls, Flick yields control back to Epic, which executes tools and re-invokes Flick with results. Epic handles orchestration, task decomposition, tool execution, and state management.

| Project | Role |
|---------|------|
| Epic | Orchestrator — recursive task decomposition, tool execution, state management, TUI |
| Flick | Agent primitive — single-shot LLM call, tool declaration (not execution), JSON result output |

## CLI Interface

```
flick run --config <toml> [--query <text>] [--resume <hash>] [--tool-results <json>] [--dry-run] [--model <id>] [--reasoning <level>]
flick setup <provider>
flick init [--output <path>]
flick list
```

- `run`: make a single model call, write JSON result to stdout
- `setup`: interactive credential onboarding per provider
- `init`: interactive config generator (writes `flick.toml` by default; `--output` to change path)
- `list`: show onboarded providers with API type and base URL
- Query from `--query` or stdin (new session)
- `--resume <hash>`: load prior context by hash, continue a session
- `--tool-results <json>`: tool results file for resumed session (required with `--resume`)
- `--dry-run`: dump API request as JSON, no model call
- `--query` and `--resume` are mutually exclusive

## Output Format (single JSON result)

Each invocation writes one JSON object to stdout. The `status` field tells the caller what to do next.

**Tool calls pending** (caller must execute tools and resume):
```json
{
  "status": "tool_calls_pending",
  "content": [
    {"type": "text", "text": "I'll read that file."},
    {"type": "tool_use", "id": "tc_1", "name": "read_file", "input": {"path": "src/main.rs"}}
  ],
  "usage": {"input_tokens": 1200, "output_tokens": 340, "cache_creation_input_tokens": 800, "cache_read_input_tokens": 400, "cost_usd": 0.0087},
  "context_hash": "00a1b2c3d4e5f67890abcdef12345678"
}
```

**Complete** (no further action):
```json
{
  "status": "complete",
  "content": [{"type": "text", "text": "Done."}],
  "usage": {"input_tokens": 2400, "output_tokens": 50, "cost_usd": 0.0032},
  "context_hash": "11b2c3d4e5f67890abcdef1234567899"
}
```

**Error:**
```json
{"status": "error", "error": {"message": "Rate limit exceeded", "code": "rate_limit"}}
```

The `usage` fields `cache_creation_input_tokens` and `cache_read_input_tokens` are omitted when zero.

## Invocation Model (single-shot)

Each `flick run` makes exactly one model call and returns. The caller drives the loop.

1. Build request (tools from config, messages from context)
2. Call provider
3. Append assistant message to context
4. Write context file, compute hash
5. Return JSON result with `status`:
   - `tool_calls_pending` → caller executes tools, resumes with `--resume <hash> --tool-results <file>`
   - `complete` → session finished
   - `error` → invocation failed

## Design Principles

- **Ultra-small.** Minimal binary, minimal dependencies (15 runtime crates (+1 Windows-only)).
- **Ultra-fast.** Negligible startup overhead. Time-to-first-token is the bottleneck.
- **Unix-philosophy.** Takes input, produces output, composes via stdin/stdout.
- **No framework.** Single executable, not an SDK or library.
- **Tool-calling models only.** No capability-checking fallbacks.
- **Compatibility-by-configuration.** Provider quirks via flags, not subclasses.
- **Separation of concerns.** Flick is a pure LLM interface: config in, model call, result out. Tool execution is the caller's responsibility.
- **Monadic / single-shot.** One invocation = one model call = one JSON result. The caller composes invocations into an agent loop.

## Provider Support

| API Type | Providers |
|----------|-----------|
| **Messages API** (native) | Anthropic (Claude) |
| **Chat Completions** | OpenAI, OpenRouter, Groq, Mistral, Ollama, DeepSeek, etc. |

## Run History

After each successful (non-dry-run) invocation, Flick records:

- **`~/.flick/history.jsonl`** — one JSON object per line capturing timestamp, invocation args, token stats, and a context hash.
- **`~/.flick/contexts/{hash}.json`** — serialized conversation context, keyed by xxh3-128 hash (content-addressable dedup).

History writes are non-fatal — failures produce a stderr warning without affecting the exit code.

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
| [INIT_COMMAND.md](INIT_COMMAND.md) | `flick init` interactive config generator spec |
| [REVIEW_FINDINGS.md](REVIEW_FINDINGS.md) | Open issues and fix-later items |
| [MONADIC_TOOLS.md](MONADIC_TOOLS.md) | Monadic architecture design spec |
