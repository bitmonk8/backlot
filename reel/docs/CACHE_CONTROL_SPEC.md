# Cache Control Spec

**Status:** Spec complete — 2-breakpoint approach, flick-only implementation, no open questions
**Scope:** flick (LLM client library) — reel benefits transparently, no changes needed
**Date:** 2026-03-27

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

Flick owns request construction and API serialization. Since the 2-breakpoint
strategy is positional (system block + last user message), flick can inject
breakpoints at serialization time. Reel and other consumers benefit
transparently — no consumer-side changes needed.

---

## Background: Cache Control Mechanisms

### Explicit Breakpoints (`cache_control`)

Opt-in via `cache_control: {"type": "ephemeral"}` on content blocks.

- Placeable on: system text blocks, tool definitions, user/assistant message
  content blocks, tool_result blocks.
- Up to **4 breakpoints** per request.
- Prefix order in the request body: **tools -> system -> messages**. A
  change at any position invalidates caching for that position and
  everything after it (the prefix must match byte-for-byte from the start).
- **5-minute TTL**, refreshed on each cache hit. Extended 1-hour TTL
  available (`"ttl": "1h"`) at 2x base input write cost (vs 1.25x for
  5-minute).
- Cache write costs 1.25x base input. Cache read costs 0.1x base input.
  Break-even at ~2-3 cache hits.
- Exact prefix match required (byte-level). Any difference in the cached
  prefix (whitespace, ordering, content) causes a full miss.
- **Minimum cacheable tokens**: 1,024 (Sonnet 4.5), 2,048 (Sonnet 4.6,
  Haiku 3.5), 4,096 (Opus 4.5/4.6, Haiku 4.5). Undersized breakpoints are
  silently ignored.
- Response reports `cache_creation_input_tokens` and
  `cache_read_input_tokens` in usage.

Supported by:
- **Anthropic** (Messages API, native). Full 4-breakpoint support.
- **Google Gemini** (via OpenRouter and LiteLLM). Same `cache_control`
  format, but only the **final breakpoint** is used. Min 1,024 tokens.
- **Bedrock** (via LiteLLM). Uses Bedrock's `cachePoint` API under the hood.

Example (Anthropic Messages API):

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

### Automatic (Implicit) Caching

Some providers cache prefixes automatically with no `cache_control`
annotations needed. `cache_control` annotations sent to these providers
are ignored (safe to include unconditionally).

- **OpenAI** — automatic prefix caching for prompts > 1,024 tokens.
- **DeepSeek** — automatic, no configuration needed.
- **Grok, Moonshot AI, Groq** — automatic via OpenRouter.

### Provider Behavior Summary

| Provider | Mechanism | Notes |
|---|---|---|
| Anthropic (direct) | Explicit breakpoints | Up to 4, 5-min or 1-hour TTL |
| Gemini / Vertex AI | Explicit breakpoints | Only final breakpoint used, min 1,024 tokens |
| OpenAI | Automatic | No annotations needed, ignored if sent |
| DeepSeek | Automatic | No annotations needed |
| Bedrock | Explicit (`cachePoint`) | LiteLLM translates `cache_control` to `cachePoint` |

### OpenRouter

OpenRouter supports both explicit and implicit caching across providers via
its Chat Completions endpoint:

- Passes `cache_control` through to Anthropic and Gemini models.
- Also supports a top-level `cache_control` object for automatic prefix
  caching and optional `"ttl": "1h"` (Anthropic direct only).
- **Sticky routing**: hashes the first system message and first user message
  to route subsequent requests to the same provider endpoint, maximizing
  cache hits. Activates only when cache read pricing < standard prompt
  pricing.
- **Detecting cache support**: The `/api/v1/models` endpoint includes
  `input_cache_read` and `input_cache_write` pricing fields on models that
  support caching. Absence of these fields indicates no cache support.
- Response reports cache usage via `prompt_tokens_details.cached_tokens`
  and `prompt_tokens_details.cache_write_tokens`.

### LiteLLM

LiteLLM supports prompt caching for: Anthropic, OpenAI, Gemini/Vertex AI,
Bedrock, and DeepSeek.

- **Anthropic / Gemini / Vertex / Bedrock**: `cache_control` annotations on
  content blocks are passed through (same format for all).
- **OpenAI / DeepSeek**: Automatic caching, no annotations needed.
- **Unsupported providers**: `cache_control` annotations are **ignored**
  (not rejected), so code can include them unconditionally.
- Auto-injection available via `cache_control_injection_points` parameter
  (e.g., `[{"location": "message", "role": "system"}]`).

---

## Breakpoint Strategy

**2-breakpoint approach** (modeled after Pi Agent, badlogic/pi-mono):

1. **System prompt** — `cache_control` on the system text block. Flick
   serializes the system prompt as a single content block (the
   `system_prompt` field is `Option<String>`). One breakpoint.
2. **Last user message** — `cache_control` on the last content block of the
   last `user`-role message. One breakpoint.

No breakpoints on tool definitions — they fall within the cached prefix
naturally since tools precede messages in the API payload. No breakpoints on
intermediate conversation messages.

This uses 2 of the 4 available breakpoints. The remaining 2 are available for
future use if needed (e.g., large injected context, tool result caching in
long tool loops).

### Cache Retention

Configurable per-request, modeled after Pi's `CacheRetention`:

- `"short"` (default): standard 5-minute ephemeral TTL.
- `"long"`: adds `ttl: "1h"` at 2x base input write cost. Only applicable
  when targeting Anthropic directly (not via OpenRouter/LiteLLM).
- `"none"`: no `cache_control` added anywhere.

### Why 2 Breakpoints

- Reel's tool definitions and system prompt are static within a session —
  they never change between turns. One breakpoint after the system prompt
  caches both tools and system as a single prefix.
- The second breakpoint on the last user message caches the growing
  conversation history. Each new turn pays only for the new message content.
- Simpler implementation than a 4-breakpoint strategy with equivalent
  cost savings for reel's architecture (no RAG/knowledge injection, no
  dynamic tool sets).

### Cache Matching Mechanism

**Breakpoints and matching are separate operations.** Breakpoints determine
where cache entries are *created*. Matching determines which cached prefix
is *read*. The two do not need to align.

- **Entry creation**: when a request includes `cache_control` at a content
  block, the API caches the prefix from the start of the request up to
  (and including) that block. Each breakpoint creates one cache entry.
- **Matching**: on a subsequent request, the API finds the **longest cached
  prefix that matches the beginning of the new request** — regardless of
  where breakpoints are in the new request. From Anthropic's docs: *"Cache
  lookup is performed based on exact prefix matching. The system will find
  the longest matching prefix among the available cache entries."*

This distinction is critical for understanding why the 2-breakpoint
strategy works across turns even though BP2's position moves:

```
Turn 1: request = [T+S(BP1), U1(BP2)]
  Creates entry A: [T+S]           (10k bytes)
  Creates entry B: [T+S, U1]      (11k bytes)
  cache_read=0, cache_write=11k

Turn 2: request = [T+S(BP1), U1, A1, U2(BP2)]
  Request starts with [T+S, U1, ...] — first 11k bytes match entry B
  Longest match: entry B (11k)     ← not at a BP position in this request
  cache_read=11k, cache_write=3k (A1+U2)
  Creates entry C: [T+S, U1, A1, U2]  (14k bytes)

Turn 3: request = [T+S(BP1), U1, A1, U2, A2, U3(BP2)]
  Request starts with [T+S, U1, A1, U2, ...] — first 14k bytes match entry C
  Longest match: entry C (14k)
  cache_read=14k, cache_write=3k (A2+U3)
```

On turn 2, BP2 is at position 14k, but the cache hit is at position 11k
(from turn 1's BP2 entry). The match position does not need to coincide
with a breakpoint in the current request. BP2's role in turn 2 is to
create a *new* cache entry at 14k, which turn 3 then reads.

### Multi-Turn Cost Analysis

Using: T+S (tools+system) = 10k, U (user message) = 1k, A (assistant
message) = 2k. Rates: cache_read = 0.1x, cache_write = 1.25x,
uncached = 1.0x.

**3-turn tool loop:**

| Turn | Total input | Cache read | Cache write | Uncached | Effective cost |
|------|------------|------------|-------------|----------|----------------|
| 1 | 11k | 0 | 11k | 0 | 11k × 1.25 = 13.75k |
| 2 | 14k | 11k | 3k | 0 | 11k × 0.1 + 3k × 1.25 = 4.85k |
| 3 | 17k | 14k | 3k | 0 | 14k × 0.1 + 3k × 1.25 = 5.15k |
| **Total** | **42k** | | | | **23.75k (43% savings vs 42k uncached)** |

Each turn pays cache_write only for the new delta (one assistant message +
one user message). The growing prefix is a cache read. Savings improve with
more turns and larger static prefixes.

A 3-breakpoint strategy (system + second-to-last user + last user) produces
identical costs — the longest cached prefix is always the previous turn's
last-user-message entry, making the additional breakpoint redundant. The
extra entry would only help as a fallback if the longest entry's 5-minute
TTL expired, which does not occur in reel's rapid tool-loop pattern.

### Prefix Stability Requirements

Cache hits require byte-identical prefixes across turns. The relevant
question is not whether serialization is deterministic in general, but
whether the *same logical content* produces the *same bytes* every time
flick serializes a request. Neither Pi Agent nor the proposed flick
implementation uses explicit JSON canonicalization — both rely on their
language runtime's deterministic behavior.

**Why flick's serialization is stable:**

- `serde_json` without the `preserve_order` feature uses `BTreeMap` for
  JSON object keys. BTreeMap always produces alphabetical key ordering,
  regardless of insertion order. This is stronger than JavaScript's
  insertion-order preservation (which Pi Agent relies on) — BTreeMap's
  output depends only on the key set, not on code path.
- `serde_json::json!()` macros in `build_body()` produce the same
  `Value::Object(BTreeMap)` on every call with the same inputs.
- Float serialization (`temperature: f32`) uses serde_json's deterministic
  formatter. Same f32 value → same string representation.
- Tool definition `input_schema` fields are `serde_json::Value`. Even if
  constructed from nondeterministic sources (e.g., HashMap), the resulting
  BTreeMap sorts keys on construction.

**What actually breaks caching (content-level, not serialization-level):**

- **Nondeterministic content in system prompts.** Timestamps, random IDs,
  or session-unique data in the system prompt text change the prefix on
  every request. Pi Agent had this bug — the system prompt included the
  current time. Fixed by using date-only: `new Date().toISOString().slice(0, 10)`
  (Issue #2131). Reel consumers must ensure system prompts contain no
  per-request varying content (or vary only at the end, after the cache
  breakpoint, which is not possible with a single system block).
- **Tool definitions that change between turns.** Reel's built-in tools
  are static within a session. Custom tools from `ToolHandler` must also
  be stable — the same tool set with the same schemas on every turn.
  Dynamic tool sets (adding/removing tools mid-session) invalidate the
  tools+system prefix.
- **`tool_choice` or `thinking` parameter changes.** These appear before
  messages in the request body. Changing them mid-session invalidates
  the prefix for all downstream content.
- **Flick version changes.** A flick update that adds/removes/reorders
  fields in `build_body()` changes the byte-level output. This
  invalidates caches across upgrades, not within a session. Acceptable —
  cache TTL is 5 minutes; version upgrades are infrequent.

**What does NOT break caching:**

- New messages appended to the conversation. The prefix (tools + system +
  earlier messages) remains byte-identical; only the suffix grows.
- `serde_json` BTreeMap key ordering. Deterministic by construction.
- Assistant message roundtrip re-serialization. See next section.

**No `preserve_order` feature needed.** The `serde_json` `preserve_order`
feature (IndexMap) is sometimes recommended for cache stability, but
BTreeMap is already deterministic. `preserve_order` would only matter if
code needed to match a specific non-alphabetical key order expected by
an external system — the Anthropic API does not have such expectations.

### Assistant Message Roundtrip

All current providers (Anthropic, OpenAI, Gemini, DeepSeek, Bedrock)
cache only the **input** prefix — the model response is always generated
fresh, never cached. However, assistant messages from prior turns are
included in the input on subsequent turns (as part of the messages array).
This means flick's parse → store → re-serialize roundtrip must produce
stable bytes across turns.

**Data flow per turn:**

```
Turn 1: flick sends [tools + system + user_1(bp)] → cache stores prefix
        API returns assistant_response_1 → flick parses → Context stores ContentBlocks

Turn 2: flick sends [tools + system + user_1 + asst_1_reserialized + user_2(bp)]
        cache hit on [tools + system + user_1] (same bytes as turn 1)
        cache stores full prefix through user_2

Turn 3: flick sends [tools + system + user_1 + asst_1_reserialized + user_2 + asst_2_reserialized + user_3(bp)]
        cache hit on prefix through user_2 (same asst_1_reserialized + user_2 bytes as turn 2)
```

**The cache never compares flick's serialization against the API's
original response bytes.** It compares flick's serialization on turn N
against flick's serialization on turn N+1. The same `ContentBlock` data
in `Context` always serializes to the same bytes via `convert_message()`.

**Roundtrip details per ContentBlock variant:**

- **Text**: `{"type":"text","text":"..."}` → `ContentBlock::Text { text }`
  → same JSON. Lossless.
- **Thinking**: API returns `"thinking"` field name, flick maps to internal
  `text` field, `convert_message()` maps back to `"thinking"`. Lossless.
- **ToolUse**: `input` field takes a double parse: API response →
  `serde_json::Value` → String (`ModelResponse.arguments`) →
  `serde_json::Value` (`build_content`). The second parse canonicalizes
  keys into BTreeMap (alphabetical). All subsequent serializations produce
  identical bytes.
- **ToolResult**: `tool_use_id`, `content`, `is_error`. Lossless.
- **Unknown**: Raw `serde_json::Value` passthrough. Keys canonicalized to
  BTreeMap on parse; stable thereafter.

**The re-serialized assistant message may differ from what the API
originally returned** (e.g., `tool_use.input` keys reordered
alphabetically). This does not matter — the cache only compares
flick-serialized bytes against flick-serialized bytes from a later turn.
Self-consistency is sufficient; lossless fidelity to the API's response
format is not required.

**Minimum token thresholds** apply: very short system prompts or tool lists
may not be cacheable on their own. The API silently ignores undersized
breakpoints with no extra cost. Flick should inject `cache_control`
unconditionally and not attempt client-side token counting. (Pi Agent
does the same — no threshold checks.)

### Provider Compatibility

The 2-breakpoint strategy (`cache_control` on system + last user message)
is safe to send unconditionally across all providers:

- **Anthropic (direct)**: both breakpoints used as intended.
- **Gemini / Vertex**: only the final breakpoint (last user message) takes
  effect. System prompt breakpoint is silently ignored.
- **OpenAI / DeepSeek**: `cache_control` annotations are ignored; automatic
  prefix caching applies regardless.
- **OpenRouter**: passes breakpoints through to Anthropic/Gemini; sticky
  routing maximizes cache hits for all providers.
- **LiteLLM**: passes breakpoints through for supported providers; ignores
  them for unsupported ones.

---

## Current State in Flick and Reel

### Flick

Flick's `RequestConfig` and `FlickClient` do not surface `cache_control` on
any content blocks. The request builder serializes tools, system, and messages
without cache annotations. The `Usage` struct already tracks
`cache_creation_input_tokens` and `cache_read_input_tokens` (these fields
exist but are always zero because no breakpoints are sent).

Key structural facts in flick that support serialization-time injection:

- **Messages API provider** (`provider/messages.rs`) already serializes the
  system prompt as a content block array:
  `body["system"] = json!([{"type": "text", "text": system}])`. Adding
  `cache_control` to this block is a one-line change.
- **`convert_message()`** serializes each message's `ContentBlock` variants
  individually. The last block of the last user message is identifiable at
  serialization time.
- **Chat Completions provider** (`provider/chat_completions.rs`) uses a
  flatter structure (plain strings, separate tool-role messages). It does
  not use content block arrays that carry `cache_control`. This is fine —
  OpenAI/DeepSeek cache automatically, and OpenRouter/LiteLLM pass through
  content-block annotations only for Anthropic-backed models.

### Reel

Reel's `Agent` builds requests via `build_request_config` and delegates to
flick. The tool loop calls `client.run()` and `client.resume()` without
cache awareness. Reel's `Usage` struct already propagates flick's cache
token fields.

### What needs to change

**Flick only.** The 2-breakpoint strategy is fully implementable at flick's
serialization layer. Reel requires no changes — it benefits transparently.

Flick changes:

1. **`RequestConfig`** — add `cache_retention: CacheRetention` field
   (default `Short`). Threaded through `runner::build_params()` into
   `RequestParams`.
2. **Messages API provider** (`build_body`) — inject `cache_control` on:
   - The system text block (breakpoint 1).
   - The last content block of the last user message (breakpoint 2).
   - When `cache_retention` is `Long`, add `"ttl": "1h"` (direct Anthropic
     only — see TTL scoping below).
   - When `cache_retention` is `None`, skip injection entirely.
3. **Chat Completions provider** — no changes. Annotations are not
   applicable to the flat message format. Automatic caching by
   OpenAI/DeepSeek is unaffected.

Why reel needs no changes:

- `run()` pushes a user Text message, then builds the request. Flick
  injects breakpoints on the system block and that last user message.
- `resume()` pushes a user ToolResult message, then builds the same way.
  The system block keeps its breakpoint; the new tool-results message
  gets the second breakpoint. The conversation history between them is
  cached by prefix match.
- Reel's tool loop alternates `run()` / `resume()` — same path each time.
- Cache usage fields (`cache_creation_input_tokens`,
  `cache_read_input_tokens`) already flow through `Usage` unchanged.

---

## Comparison with Pi Agent

Pi Agent (badlogic/pi-mono) implements the same 2-breakpoint strategy. The
architecture aligns closely with the proposed flick implementation.

### Same approach

| Aspect | Pi Agent | Proposed (flick) |
|---|---|---|
| Breakpoint 1 | System prompt first block | System text block |
| Breakpoint 2 | Last block of last user message | Last block of last user message |
| Injection point | Serialization time (`buildParams()` / `convertMessages()`) | Serialization time (`build_body()`) |
| Stored on messages? | No — derived from position at request time | No — same |
| Config type | `CacheRetention = "none" \| "short" \| "long"` | `CacheRetention { None, Short, Long }` enum |
| Default | `"short"` | `Short` |
| Token threshold checks | None — injects unconditionally | None — same |
| Survives resume? | Yes — re-derived each request | Yes — same |
| JSON canonicalization | None — relies on JS insertion-order preservation | None — relies on serde_json BTreeMap (alphabetical) |
| Prefix stability testing | Multi-turn probe validates `cache_read` tokens increase monotonically | Not yet implemented — should adopt same pattern |

### Differences

| Aspect | Pi Agent | Proposed (flick) |
|---|---|---|
| TTL scoping | URL detection (`baseUrl.includes("api.anthropic.com")`) — TTL only for direct Anthropic | Flick knows the provider via `ApiKind` enum — no URL parsing needed |
| OpenAI caching | Uses `prompt_cache_key` (sessionId) + `prompt_cache_retention` on OpenAI Responses API | Not applicable — flick targets Chat Completions, where OpenAI caches automatically |
| Bedrock | Explicit `CachePoint` blocks via AWS SDK | Not currently a flick provider target |
| Global override | `PI_CACHE_RETENTION` env var | Not planned — per-`RequestConfig` is sufficient |
| Per-session ID | `sessionId` property on Agent, threaded to OpenAI's `prompt_cache_key` | Not needed — no OpenAI Responses API path in flick |
| Serialization determinism | JS string key insertion order (ES2015+ guarantee). Weaker — depends on code path consistency. | serde_json BTreeMap alphabetical ordering. Stronger — depends only on key set, not insertion order. |

### Prefix stability: Pi Agent lessons

Pi Agent encountered one prefix stability bug worth noting:

- **Issue #2131**: The system prompt included the current time
  (`new Date().toISOString()`), causing the prefix to change on every
  request. Fixed by truncating to date-only:
  `new Date().toISOString().slice(0, 10)`. Reel consumers must apply the
  same discipline — no per-request varying content in system prompts.

Pi Agent validates cache effectiveness with a multi-turn probe test
(`sdk-codex-cache-probe-tool-loop.ts`) that asserts `cache_read_input_tokens`
is monotonically increasing across turns in a tool loop. This is a useful
pattern for flick to adopt — it catches serialization regressions that
unit tests would miss.

### Pi Agent reference code

- Type: `packages/ai/src/types.ts:56` — `CacheRetention`
- Anthropic injection: `packages/ai/src/providers/anthropic.ts:623-635` (system), `:841-862` (last user message)
- TTL resolution: `packages/ai/src/providers/anthropic.ts:49-62` — `getCacheControl()`
- OpenAI Responses: `packages/ai/src/providers/openai-responses.ts:41-49` — `prompt_cache_key` / `prompt_cache_retention`
- Cache probe test: `packages/coding-agent/test/sdk-codex-cache-probe-tool-loop.ts`
- System prompt fix: `packages/coding-agent/src/core/system-prompt.ts:42`
- Tests: `packages/ai/test/cache-retention.test.ts`

---

## Resolved Questions

1. **TTL scoping.** `ApiKind::Messages` gating is sufficient. Both
   OpenRouter and LiteLLM pass `cache_control` objects (including `"ttl":
   "1h"`) verbatim to Anthropic when the request uses Anthropic's native
   content-block format — which is `ApiKind::Messages` in flick. When a
   proxy uses Chat Completions format, the flat message structure has no
   content-block arrays to carry `cache_control`, so the question of TTL
   passthrough does not arise. No additional flags or URL-based detection
   needed.

2. **Warm-up calls.** Not needed. Reel's tool loop is multi-turn, so the
   first real call primes the cache for subsequent turns. Pi Agent does not
   use warm-up calls either.

---

## Next Steps

1. Add `CacheRetention` enum and `cache_retention` field to flick's
   `RequestConfig`.
2. Inject breakpoints in flick's Messages API provider (`build_body`):
   system block + last user message last block.
3. Gate `"ttl": "1h"` on `ApiKind::Messages` + `CacheRetention::Long`.
4. Add tests: verify breakpoint placement, TTL scoping, `None` skips
   injection, Chat Completions path is unaffected.
5. Verify reel's existing tests pass unchanged (no reel modifications).
