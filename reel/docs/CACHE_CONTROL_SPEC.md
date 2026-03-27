# Cache Control Spec

**Status:** Exploring problem and solution space
**Scope:** flick (LLM client library) and reel (agent runtime)
**Date:** 2026-03-26

---

## Problem

Reel and flick do not use cache control mechanisms provided by the Anthropic
Messages API or any equivalent mechanism for the Chat Completions API. Every
request re-sends the full prompt prefix (tools, system prompt, conversation
history) as uncached input tokens.

Consequences:

- **Cost.** Each turn pays full input token price for the entire prefix.
  Cache reads cost 0.1x base input price (90% savings). A 10-turn
  conversation with a 50k-token prefix pays ~10x more than one that caches
  the prefix after the first turn.
- **Latency.** Cached prefixes skip KV-cache recomputation on the provider
  side. Anthropic documents up to 85% latency reduction for cache hits on
  long prefixes.

Both flick and reel are affected. Flick owns the request construction and API
call. Reel owns the tool loop and prompt assembly. The fix requires changes in
both projects.

---

## Background: Cache Control Mechanisms

### Anthropic Messages API

Explicit opt-in via `cache_control: {"type": "ephemeral"}` on content blocks.

- Placeable on: system text blocks, tool definitions, user/assistant message
  content blocks, tool_result blocks.
- Up to **4 breakpoints** per request.
- Lookup hierarchy (prefix order): **tools -> system -> messages**. A change
  at any level invalidates that level and all downstream caches.
- **5-minute TTL**, refreshed on each cache hit. Extended 1-hour TTL
  available (`"ttl": "1h"`) at 2x base input write cost (vs 1.25x for
  5-minute).
- **Minimum cacheable tokens**: 1,024 (Sonnet 4.5), 2,048 (Sonnet 4.6,
  Haiku 3.5), 4,096 (Opus 4.5/4.6, Haiku 4.5).
- Cache write costs 1.25x base input. Cache read costs 0.1x base input.
  Break-even at ~2-3 cache hits.
- Exact prefix match required (byte-level). Any difference in the cached
  prefix (whitespace, ordering, content) causes a full miss.
- Response reports `cache_creation_input_tokens` and
  `cache_read_input_tokens` in usage.

Example:

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "system": [
    {
      "type": "text",
      "text": "Static system instructions...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "tools": [
    {"name": "read", "...": "...", "cache_control": {"type": "ephemeral"}}
  ],
  "messages": [
    {"role": "user", "content": [
      {"type": "text", "text": "Previous user message..."}
    ]},
    {"role": "assistant", "content": [
      {"type": "text", "text": "Previous assistant response..."}
    ]},
    {"role": "user", "content": [
      {"type": "text", "text": "Current question",
       "cache_control": {"type": "ephemeral"}}
    ]}
  ]
}
```

### Chat Completions API (OpenRouter, LiteLLM)

**OpenRouter** supports `cache_control` on content blocks for Anthropic models
routed through their Chat Completions endpoint. Same 4-breakpoint limit. Also
supports a top-level `cache_control` object for automatic prefix caching and
optional `"ttl": "1h"`. Uses provider sticky routing to maximize cache hits.

**LiteLLM** passes `cache_control` through to Anthropic when using
`anthropic/claude-*` model prefixes. Filters it out for providers that do not
support it.

**OpenAI native** uses automatic prefix-based caching with no explicit
markers. No action needed for OpenAI models.

---

## Reference: How Existing Agents Handle Caching

### Pi Agent (badlogic/pi-mono)

Pi uses **2 breakpoints**:

1. **System prompt** -- `cache_control` on each system text block.
2. **Last user message** -- `cache_control` on the last content block of the
   last `user`-role message.

No breakpoints on tool definitions (they fall within the cached prefix
naturally since tools precede messages in the API payload). No breakpoints on
intermediate conversation messages.

Pi also supports configurable cache retention:
- `"short"` (default): standard 5-minute ephemeral TTL.
- `"long"`: adds `ttl: "1h"` (only when base URL is `api.anthropic.com`).
- `"none"`: no `cache_control` added anywhere.

The OpenRouter/OpenAI Completions path uses a single breakpoint on the last
user/assistant message. Bedrock provider follows the same 2-breakpoint pattern
using Bedrock's `cachePoint` API.

Source: `packages/ai/src/providers/anthropic.ts` (lines 624-646, 841-862),
`packages/ai/src/types.ts` (line 56).

### Claude Code

Uses up to **4 breakpoints** in a structured strategy:

1. **Last tool definition** -- caches all tools as a block.
2. **System prompt** -- caches static instructions + injected context
   (CLAUDE.md, skills).
3. **RAG/knowledge context** -- caches injected files and skill definitions.
4. **Last conversation message** -- caches the growing history prefix.

Additional strategies:
- Stable prefix ordering (byte-identical tools/system across turns).
- Breakpoint moves forward as conversation grows.
- Warm-up calls to prime the cache before the first real user interaction.
- Avoids parameter changes (`tool_choice`, `thinking`) that invalidate
  upstream caches.

---

## Breakpoint Placement Strategies

### Optimal content ordering (most static first)

1. Tool definitions (most stable across a session)
2. System prompt (static instructions)
3. Large reference documents / skills / injected context
4. Conversation history (grows each turn)
5. Current user message (most dynamic -- typically the breakpoint target, not
   cached itself)

### Multi-turn agentic loops

- Place a breakpoint at the end of the conversation prefix (last user or
  assistant message before the new input). Each new turn pays only for the
  new message.
- For long conversations, use two breakpoints: one after system prompt, one
  at end of conversation history.
- Tool results can carry `cache_control` to extend the cached prefix through
  tool call/result cycles.

### Gotchas

- Exact byte match required for prefix. Nondeterministic formatting
  (floating-point serialization, map key ordering) breaks caching.
- Cache entries only available after the first response begins streaming.
  Parallel requests cannot share a cache write that has not completed.
- Changes to `tool_choice` or `thinking` parameters invalidate system and
  message caches.
- Minimum token thresholds mean very short system prompts or tool lists may
  not be cacheable on their own.

---

## Current State in Flick and Reel

### Flick

Flick's `RequestConfig` and `FlickClient` do not surface `cache_control` on
any content blocks. The request builder serializes tools, system, and messages
without cache annotations. The `Usage` struct already tracks
`cache_creation_input_tokens` and `cache_read_input_tokens` (these fields
exist but are always zero because no breakpoints are sent).

### Reel

Reel's `Agent` builds requests via `build_request_config` and delegates to
flick. It has no mechanism to annotate content blocks with `cache_control`.
The tool loop calls `client.run()` and `client.resume()` without cache
awareness. Reel's `Usage` struct already propagates flick's cache token
fields.

### What needs to change

Both layers need work:

- **Flick** must support `cache_control` annotations on content blocks in
  its request types and serialize them correctly for both Messages API and
  Chat Completions API (OpenRouter/LiteLLM passthrough).
- **Reel** must implement a breakpoint placement strategy in its tool loop
  and request construction, deciding where to place breakpoints based on
  content stability.

---

## Open Questions

1. **Breakpoint strategy for reel.** Pi's 2-breakpoint approach (system +
   last user message) is simple and effective. Claude Code's 4-breakpoint
   approach is more granular. Which fits reel's architecture better?

2. **Flick API surface.** How should `cache_control` be exposed? Options:
   - Automatic placement by flick based on content structure.
   - Manual placement by consumers (reel) via annotated content blocks.
   - Hybrid: flick provides defaults, consumers can override.

3. **Chat Completions compatibility.** Flick supports multiple providers.
   Cache control must be provider-aware (Anthropic: explicit breakpoints,
   OpenAI: automatic, OpenRouter: passthrough). How to abstract this?

4. **Cache retention configuration.** Should reel/flick support configurable
   TTL (5-min vs 1-hour)? Pi's `CacheRetention` enum is a reasonable model.

5. **Tool definition stability.** Reel's tool definitions are static within a
   session. Should we always cache them (breakpoint on last tool)?

6. **Minimum token thresholds.** Should flick skip `cache_control` when the
   content block is below the model's minimum cacheable token count? Or leave
   that to the API (which silently ignores undersized breakpoints)?

7. **Warm-up calls.** Should reel's `Agent::run()` perform a warm-up call to
   prime the cache before the first real tool loop iteration?

---

## Next Steps

- Explore flick's request types and determine the minimal API surface change
  needed to support `cache_control` annotations.
- Prototype a 2-breakpoint strategy (system + last message) in reel's tool
  loop and measure cost/latency impact.
- Investigate Chat Completions passthrough behavior for OpenRouter and
  LiteLLM with flick's provider abstraction.
