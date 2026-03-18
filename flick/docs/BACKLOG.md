# Flick — Backlog

20 items in 5 active clusters, ordered by value (highest first).

Original IDs (L*n*, T*n*) preserved for traceability. Severity markers: **M** = medium, **L** = low.

---

## 1. Context & Serialization Robustness (4 items)

Unknown content block types, empty content vecs, message ordering validation, missing serde defaults. All in `context.rs`.

### L7. `ContentBlock` has no unknown-variant fallback — `context.rs:29-55`

Deserialization of an unknown `type` field (e.g., `{"type":"image"}`) produces a hard error. If a future provider or persisted context file contains an unfamiliar content block type, `Context::load_from_file` fails entirely.

- **M** — Fix Risk: Low — Effort: Low

### T24. `push_assistant` accepts empty content vec — `context.rs`

Caller guards against this, but the method itself does not validate. Empty assistant message would violate API constraints.

- **L** — Fix Risk: None — Effort: Trivial

### T75. `load_from_file` does not validate message ordering — `context.rs`

Deserialised contexts bypass all push-method invariants. A persisted file could contain two consecutive `Assistant` messages, misplaced `ToolResult` blocks, or an assistant-first sequence. The API would reject the malformed history with an opaque error rather than a clear validation message.

- **L** — Fix Risk: Medium — Effort: Low

### T78. `Message.content` missing `#[serde(default)]` — `context.rs`

A serialised message with the `content` key absent fails deserialisation with "missing field". Externally produced or hand-edited context files may omit it.

- **L** — Fix Risk: None — Effort: Trivial

---

## 2. Error Type Hygiene (5 items)

Overloaded error variants, wrong variant names, misattributed JSON errors. One sweep through `error.rs` and its consumers.

### L4. `CredentialError::InvalidFormat` overloaded for 6+ distinct failure modes — `provider_registry.rs`, `error.rs`

`InvalidFormat(String)` covers: hex decode failures, key length errors, TOML parse errors, encryption failures, and Windows API errors. Makes programmatic error handling and debugging harder.

- **L** — Fix Risk: Low — Effort: Low

### T25. `FlickError::code()` couples to `ProviderError` internals — `error.rs`

`FlickError::code()` must know about all `ProviderError` variants rather than delegating to `ProviderError::code()`.

- **L** — Fix Risk: Low — Effort: Low

### T76. `ProviderError` missing `InvalidRequest` variant — `error.rs`

`chat_completions::validate_params` uses `ResponseParse` for client-side validation errors (e.g., tools + output_schema mutual exclusion). The root cause is the absence of an `InvalidRequest(String)` (or `ValidationFailed`) variant.

- **L** — Fix Risk: Low — Effort: Low

### T77. `From<serde_json::Error>` maps all JSON errors to `ContextParse` — `error.rs`

Any `serde_json::Error` propagated via `?` in a `FlickError` context becomes `FlickError::ContextParse`, even when unrelated to context parsing. The variant name misleads callers handling JSON errors from non-context paths.

- **L** — Fix Risk: Low — Effort: Low

### T86. `load_config` helper swallows `ConfigError` in expect message — `tests/common/mod.rs`

`.expect("config should parse")` masks the actual `ConfigError` variant and message on test failure.

- **L** — Fix Risk: None — Effort: Trivial

---

## 3. Provider — Messages API & Architecture (4 items)

Temperature+thinking guard, system prompt as array (for caching), tool_choice support, provider trait coherence. `messages.rs` and `provider.rs`.

### T10. `build_body` does not enforce temperature + thinking mutual exclusion — `messages.rs`

Anthropic API rejects requests with both `temperature` and thinking enabled. `build_body` does not guard; caller enforcement exists but is not defensive.

- **L** — Fix Risk: None — Effort: Trivial

### T54. System prompt serialised as plain string, blocking prompt caching — `messages.rs`

`body["system"] = json!(system)` produces a JSON string. The Anthropic API also accepts `system` as an array of content blocks, which is required to attach `cache_control` headers for prompt caching.

- **L** — Fix Risk: Low — Effort: Low

### T55. No `tool_choice` configuration surface for Messages provider — `messages.rs`

The Messages provider always omits `tool_choice`, relying on the Anthropic default of `auto`. There is no way to force `{"type": "any"}` or a specific tool.

- **L** — Fix Risk: Low — Effort: Low

### T65. Blanket/manual `DynProvider` coherence trap undocumented — `provider.rs`

`ProviderInstance` has a manual `DynProvider` impl, coexisting with the blanket `impl<T: Provider> DynProvider for T`. If someone later adds `impl Provider for ProviderInstance`, the compiler will reject both impls as conflicting. No comment warns of this constraint.

- **L** — Fix Risk: None — Effort: Trivial

---

## 4. CLI Input Handling (4 items)

Stdin size limits, provider name/key validation, whitespace-only input messages. All in `main.rs`.

### T1. `read_stdin` accepts unlimited input size — `main.rs`

`read_to_string` reads all of stdin with no size cap. A multi-gigabyte pipe causes OOM before the LLM API rejects it.

- **M** — Fix Risk: Low — Effort: Trivial

### T2. `cmd_setup_core` does not validate provider name length — `main.rs`

Extremely long provider name passes validation but fails at OS level with an unhelpful error. Add `|| provider_name.len() > 255`.

- **L** — Fix Risk: None — Effort: Trivial

### T3. `cmd_setup_core` does not validate API key content — `main.rs`

No length cap or control character check on the API key value.

- **L** — Fix Risk: None — Effort: Trivial

### T40. Whitespace-only stdin produces misleading `no_query` error — `main.rs`

`read_stdin` trims the input to an empty string, which hits the `NoQuery` path. The error message "use --query or pipe to stdin" is misleading when the user *did* pipe to stdin (but sent only whitespace).

- **L** — Fix Risk: None — Effort: Trivial

---

## 5. Test Coverage Gaps (3 items)

Missing tests for context overflow, credential edge cases, destructive mock reads, integration history verification. Independent items but suitable for a single test-writing session.

### T35. `MockProvider::captured_params()` is a destructive read — `tests/common/mod.rs`

`std::mem::take` means second call returns empty vec. Subtle footgun if reused.

- **L** — Fix Risk: None — Effort: Trivial

### T50. No test for `get()` when no secret key file exists — `provider_registry.rs` (tests)

All `get()` tests create a key via `set()` first. There is no test verifying that `get()` before any `set()` returns `CredentialError::NoSecretKey`.

- **L** — Fix Risk: None — Effort: Trivial

### T87. Context persistence tests do not verify provider received full history — `tests/integration.rs`

`end_to_end_context_persistence` and `end_to_end_context_file_loading` verify `context.messages.len()` after the second turn but do not call `captured_params()` on the provider to confirm that the full message history was transmitted.

- **M** — Fix Risk: None — Effort: Low
