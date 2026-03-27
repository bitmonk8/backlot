# Cache Control Spec

**Status:** Spec complete — ready for implementation
**Scope:** flick (LLM client library) — reel benefits transparently, no changes needed
**Date:** 2026-03-27

---

## Problem

Flick does not use cache control. Every request re-sends the full prompt
prefix (tools, system prompt, conversation history) as uncached input tokens.

- **Cost.** Cache reads cost 0.1x base input price (90% savings). A 10-turn
  conversation with a 50k-token prefix pays ~10x more than one that caches
  the prefix after the first turn.
- **Latency.** Cached prefixes skip KV-cache recomputation. Anthropic
  documents up to 85% latency reduction on long prefixes.

---

## Solution: 2-Breakpoint Strategy

Inject `cache_control: {"type": "ephemeral"}` at two positions in every
Messages API request:

1. **System prompt** — on the system text block. Tools precede system in the
   API payload, so this breakpoint caches tools + system as a single prefix.
2. **Last user message** — on the last content block of the last `user`-role
   message. Caches the growing conversation history. Each new turn pays only
   for the new message delta.

No breakpoints on tool definitions or intermediate messages. Uses 2 of 4
available breakpoints.

### Cache Retention

`CacheRetention` enum, configurable per-request:

- **`Short`** (default): standard 5-minute ephemeral TTL.
- **`Long`**: adds `"ttl": "1h"` at 2x base input write cost. Gated on
  `ApiKind::Messages` — both OpenRouter and LiteLLM pass TTL verbatim to
  Anthropic when using the native content-block format.
- **`None`**: no `cache_control` injected anywhere.

### How Cache Matching Works Across Turns

Breakpoints create cache entries. Matching finds the longest cached prefix
that matches the beginning of the new request — regardless of where
breakpoints are in the new request. BP2 moves on each turn, but prior
entries still match:

```
Turn 1: request = [T+S(BP1), U1(BP2)]
  Creates entries: [T+S] and [T+S, U1]
  cache_read=0, cache_write=11k

Turn 2: request = [T+S(BP1), U1, A1, U2(BP2)]
  Longest match: [T+S, U1] (from turn 1's BP2 entry)
  cache_read=11k, cache_write=3k (A1+U2)

Turn 3: request = [T+S(BP1), U1, A1, U2, A2, U3(BP2)]
  Longest match: [T+S, U1, A1, U2] (from turn 2's BP2 entry)
  cache_read=14k, cache_write=3k (A2+U3)
```

### Provider Compatibility

Safe to inject breakpoints unconditionally across all providers:

- **Anthropic (direct)**: both breakpoints used.
- **Gemini / Vertex**: only the final breakpoint takes effect.
- **OpenAI / DeepSeek**: `cache_control` ignored; automatic caching applies.
- **OpenRouter / LiteLLM**: pass through to supported providers, ignore for
  unsupported ones.

### Prefix Stability

Cache hits require byte-identical prefixes across turns.

**Flick's serialization is stable.** `serde_json` uses `BTreeMap`
(alphabetical key order) — deterministic regardless of insertion order.
`json!()` macros, float formatting, and tool schema `Value` fields all
produce identical bytes for identical inputs. No explicit JSON
canonicalization or `preserve_order` feature needed.

**Assistant message roundtrip is stable.** The cache compares flick's
serialization on turn N against flick's serialization on turn N+1 (not
against the API's original response bytes). `ContentBlock` data in `Context`
always re-serializes to the same bytes via `convert_message()`. ToolUse
`input` keys are canonicalized to BTreeMap on first parse.

**What breaks caching (consumer responsibility):**

- Nondeterministic system prompt content (timestamps, random IDs). Use
  date-only or static content.
- Tool definitions that change between turns. Built-in tools are static;
  custom `ToolHandler` tools must also be stable.
- `tool_choice` or `thinking` parameter changes mid-session.

**What does not break caching:**

- New messages appended to conversation (prefix unchanged).
- Minimum token thresholds not met — API silently ignores undersized
  breakpoints at no extra cost. Inject unconditionally; do not attempt
  client-side token counting.

---

## What Needs to Change

**Flick only.** Reel requires no changes.

### 1. `RequestConfig` — add `CacheRetention`

Add `cache_retention: CacheRetention` field (default `Short`). Thread
through `runner::build_params()` into `RequestParams`.

```rust
#[derive(Clone, Debug, Default)]
pub enum CacheRetention {
    None,
    #[default]
    Short,
    Long,
}
```

### 2. Messages API provider — inject breakpoints

In `build_body()` (`provider/messages.rs`):

- **System block.** Already serialized as a content block array:
  `body["system"] = json!([{"type": "text", "text": system}])`. Add
  `"cache_control"` to this block.
- **Last user message.** `convert_message()` serializes each `ContentBlock`
  individually. Add `"cache_control"` to the last content block of the last
  `user`-role message.
- **TTL.** When `CacheRetention::Long` and `ApiKind::Messages`: add
  `"ttl": "1h"` inside the `cache_control` object.
- **None.** When `CacheRetention::None`: skip injection entirely.

### 3. Chat Completions provider — no changes

Flat message format does not carry content-block annotations. OpenAI and
DeepSeek cache automatically.

### 4. Reel — no changes

- `run()` and `resume()` push messages, then build requests. Flick injects
  breakpoints at serialization time.
- `Usage` already propagates `cache_creation_input_tokens` and
  `cache_read_input_tokens`.

---

## Validation

1. **Unit tests** — verify breakpoint placement in serialized request body:
   system block has `cache_control`, last user message last block has
   `cache_control`, TTL present only for `Long` + `ApiKind::Messages`,
   `None` skips injection, Chat Completions path unaffected.
2. **Reel test suite** — verify existing tests pass unchanged (no reel
   modifications needed).
3. **Cache probe integration test** — multi-turn tool loop asserting
   `cache_read_input_tokens` increases monotonically across turns. Catches
   serialization regressions that unit tests would miss.
