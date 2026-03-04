# Flick — Backlog

## Fix Later (13)

### L1. No `deny_unknown_fields` — config typos silently ignored — `config.rs`

Misspelled config fields (e.g., `temprature`) are silently discarded. A user who writes `temprature = 0.5` gets no error — the temperature is `None`. Most impactful on `ModelConfig` where typos silently change behavior.

- **Severity:** Medium — **Fix Risk:** Medium — **Effort:** Low
- **Category:** Robustness

### L2. Config `base_url` not validated for URL sanity — `config.rs`

`ProviderConfig.base_url` accepts any string. Values like `file:///etc/passwd` would be passed to reqwest (which rejects non-HTTP schemes at request time with an opaque error). An `http://localhost/...` URL could be used for SSRF if flick ever runs as a service.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Security

### L3. Decrypted API keys not zeroized — `credential.rs:30-37`

`get()` returns `String` (not `Zeroizing<String>`). Plaintext API key persists in heap memory after drop. The key also exists in HTTP request headers during provider calls, so the benefit is marginal for a short-lived CLI process, but it is a gap in the defense-in-depth model.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Security

### L4. `CredentialError::InvalidFormat` overloaded for 6+ distinct failure modes — `credential.rs`, `error.rs`

`InvalidFormat(String)` covers: hex decode failures, key length errors, TOML parse errors, encryption failures, and Windows API errors. Makes programmatic error handling and debugging harder.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low
- **Category:** Quality

### L5. No handling of `refusal` field for OpenAI models — `chat_completions.rs:267-351`

OpenAI models can return a `refusal` field in `choices[0].message` instead of `content`. The `parse_response` function silently ignores it. A refusal produces an empty response with no error — the user sees nothing.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Bug

### L7. `ContentBlock` has no unknown-variant fallback — `context.rs:29-55`

Deserialization of an unknown `type` field (e.g., `{"type":"image"}`) produces a hard error. If a future provider or persisted context file contains an unfamiliar content block type, `Context::load_from_file` fails entirely.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Robustness

### L12. No test for `build_request` / dry-run path — `tests/`

The `--dry-run` code path that calls `build_request` is completely untested at the integration level. `MockProvider::build_request` returns `Ok(json!({}))` always.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low
- **Category:** Test Coverage

### L17. No test for `ProviderError::RateLimited` propagation — `tests/`

No test verifies that a provider returning `RateLimited` propagates correctly through `runner::run` as `FlickError::Provider(ProviderError::RateLimited { .. })`.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low
- **Category:** Test Coverage

### L20. Unix key-write failure leaves corrupted `.secret_key` — `credential.rs:92-111`

On the Unix code path in `load_or_create_secret_key`, if `file.write_all(hex_key.as_bytes()).await` fails the error propagates via `?` without deleting the partially-written file. Subsequent `load_secret_key` calls fail with `InvalidFormat`; `load_or_create_secret_key` retries `create_new` which hits `AlreadyExists` and re-loads the corrupt file. The Windows path correctly deletes on write failure.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial
- **Category:** Bug

### L21. `output_schema` silently ignored by Messages provider — `provider/messages.rs:96-101`

When `params.output_schema` is `Some`, the Messages provider discards it without emitting a warning. A user who configures `[output_schema]` and targets the Anthropic Messages API receives unstructured output with no diagnostic. The Chat Completions provider honours the field via `response_format`.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial
- **Category:** UX

### L22. Empty thinking signature stored in context, breaks next API call — `runner.rs`

When the provider returns thinking content with an empty signature (e.g., Chat Completions provider, or a malformed Anthropic response), a `ContentBlock::Thinking { signature: "" }` is pushed to context. When that context is later replayed to the Anthropic Messages API in a multi-turn session, the empty signature violates the API contract and produces a hard error on the next invocation.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Bug

### L23. Reasoning + `output_schema` mutual exclusion not enforced — `config.rs`

`build_params` strips temperature when reasoning is active but does not strip `output_schema`. The Anthropic API rejects requests combining extended thinking with structured output. No validation guards this combination at config load or at request build time; the error surfaces as an opaque API 400.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Bug

### L6. Cache token cost not computed — `config.rs`

`compute_cost` uses only `input_tokens`/`output_tokens`. Cache tokens (`cache_creation_input_tokens`, `cache_read_input_tokens`) are tracked in `UsageSummary` but not factored into cost. Cost will be inaccurate for cached Anthropic conversations.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Robustness

---

## Fix When Touched (37)

### T1. `read_stdin` accepts unlimited input size — `main.rs`

`read_to_string` reads all of stdin with no size cap. A multi-gigabyte pipe causes OOM before the LLM API rejects it.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial

### T2. `cmd_setup_core` does not validate provider name length — `main.rs`

Extremely long provider name passes validation but fails at OS level with an unhelpful error. Add `|| provider_name.len() > 255`.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T3. `cmd_setup_core` does not validate API key content — `main.rs`

No length cap or control character check on the API key value.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T4. Empty tool description accepted — `config.rs`

`description = ""` passes validation. Functionally useless to the model.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T6. `encrypt` does not zeroize intermediate plaintext bytes — `credential.rs`

The `plaintext` parameter is a `&str` from the caller. Same root cause as L3 from the caller side.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T7. No provider name validation in `CredentialStore::get`/`set` — `credential.rs`

Public API accepts any `&str` including TOML-special characters. Caller validates, but the store itself does not enforce invariants.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T8. `encrypt` error uses wrong variant name — `credential.rs`

`InvalidFormat("encryption failed")` — semantically wrong for an encryption operation. The error path is practically unreachable.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T10. `build_body` does not enforce temperature + thinking mutual exclusion — `messages.rs`

Anthropic API rejects requests with both `temperature` and thinking enabled. `build_body` does not guard; caller enforcement exists but is not defensive.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T11. Thinking budget silently becomes 0 when `max_tokens` < 2 — `messages.rs`

User requests reasoning but `max_tokens` is too small. Thinking is silently disabled with no warning.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T14. `convert_message` drops Thinking blocks silently for Chat Completions — `chat_completions.rs`

Thinking-only messages produce empty `"content": ""`. OpenAI accepts this but it is noise in the context window.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T17. Redundant `content-type` header — `chat_completions.rs`

`.header("content-type", "application/json")` is redundant with `.json(&body)` which sets it automatically.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T24. `push_assistant` accepts empty content vec — `context.rs`

Caller guards against this, but the method itself does not validate. Empty assistant message would violate API constraints.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T25. `FlickError::code()` couples to `ProviderError` internals — `error.rs`

`FlickError::code()` must know about all `ProviderError` variants rather than delegating to `ProviderError::code()`.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T35. `MockProvider::captured_params()` is a destructive read — `tests/common/mod.rs`

`std::mem::take` means second call returns empty vec. Subtle footgun if reused.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T40. Whitespace-only stdin produces misleading `no_query` error — `main.rs`

`read_stdin` trims the input to an empty string, which hits the `NoQuery` path. The error message "use --query or pipe to stdin" is misleading when the user *did* pipe to stdin (but sent only whitespace).

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T41. Provider name allows dot-prefixed names like `.hidden` — `main.rs`

Validation rejects exactly `"."` and `".."` but permits `.hidden`, `...`, `..foo`, etc. Dot-prefixed credential files are invisible by default on Unix.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T42. `override_reasoning` uses `take()` instead of `replace()` — `config.rs`

`override_model_name` uses `std::mem::replace` for transactional rollback; `override_reasoning` uses `take()` then assigns. If `validate()` ever panics, the field is left as `Some(new_value)` and `old` is dropped.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T43. Zero cost reported without warning for unknown models — `config.rs`

When neither a `[pricing]` config section nor a builtin registry entry exists for the model, `compute_cost` returns `0.0`. The result reports `cost_usd: 0.0`, which misleads the caller into thinking the call was free.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Low

### T44. Tool `parameters` not validated as JSON Schema — `config.rs`

`ToolConfig.parameters` accepts any `serde_json::Value` (string, number, array). An invalid schema passes config validation and is forwarded to the model; the API rejects it at request time with an opaque error.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T45. Empty `--query ""` not rejected before config/credential I/O — `main.rs`

When `--query ""` is passed explicitly, `cmd_run` loads config, decrypts credentials, and optionally reads a context file before `cmd_run_core` rejects it with `NoQuery`. The I/O is wasted and credential errors obscure the real issue.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T47. `decrypt` internals not zeroized — `credential.rs`

Inside `decrypt`, the `combined` Vec (nonce + ciphertext) and the `plaintext` Vec returned by `cipher.decrypt()` are plain allocations. The plaintext bytes survive in heap memory after drop.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T48. Temp credentials file not cleaned up on `rename` failure — `credential.rs`

If `tokio::fs::rename` fails (e.g., destination locked on Windows), the `.tmp` file containing all credentials is left on disk. It will be overwritten on the next `set()` call so there is no data-loss, but it is a robustness gap.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T49. Poly1305 authentication tag size 16 is a magic number — `credential.rs`

The minimum-length check `combined.len() < NONCE_LEN + 16` uses the literal `16` (Poly1305 tag length) without a named constant.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T50. No test for `get()` when no secret key file exists — `credential.rs` (tests)

All `get()` tests create a key via `set()` first. There is no test verifying that `get()` before any `set()` returns `CredentialError::NoSecretKey`.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T51. Secret key file not `fsync`'d before returning — `credential.rs`

Neither the Unix nor Windows path calls `sync_all()` after writing the key file. A power failure between `write_all` and OS flush leaves the file empty or truncated.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T52. Redundant `content-type` header in Messages provider — `messages.rs`

`.header("content-type", "application/json")` is redundant because `.json(&body)` sets it automatically.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T54. System prompt serialised as plain string, blocking prompt caching — `messages.rs`

`body["system"] = json!(system)` produces a JSON string. The Anthropic API also accepts `system` as an array of content blocks, which is required to attach `cache_control` headers for prompt caching.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T55. No `tool_choice` configuration surface for Messages provider — `messages.rs`

The Messages provider always omits `tool_choice`, relying on the Anthropic default of `auto`. There is no way to force `{"type": "any"}` or a specific tool.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T56. User-role `ToolUse` blocks silently dropped in `convert_message` — `chat_completions.rs`

The `has_tool_use` branch is gated on `role == "assistant"`. A `Message` with `Role::User` containing `ToolUse` blocks falls through to the text-only path, silently dropping the blocks.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T57. No `"strict": true` on tool function definitions — `chat_completions.rs`

OpenAI supports `"strict": true` on function tool definitions for schema-enforced argument generation. The provider sets `"strict": true` for `response_format` but not for tools, creating an inconsistency.

- **Severity:** Low — **Fix Risk:** Medium — **Effort:** Low

### T59. Tool result `is_error` double-prefixes "Error:" in Chat Completions — `chat_completions.rs`

Error content is wrapped as `format!("Error: {content}")`. If the caller already prefixes "Error:", the API payload becomes `"Error: Error: ..."`.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T60. `validate_params` does not reject empty messages array — `chat_completions.rs`

An empty `params.messages` would be serialised and rejected by the OpenAI API with an opaque 400 error.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T63. HTTP 408 (Request Timeout) not classified as retryable — `http.rs`

`handle_http_error` maps 408 to `ProviderError::Api`, which `classify_for_retry` treats as non-retryable. RFC 7231 explicitly permits retry on 408.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T65. Blanket/manual `DynProvider` coherence trap undocumented — `provider.rs`

`ProviderInstance` has a manual `DynProvider` impl, coexisting with the blanket `impl<T: Provider> DynProvider for T`. If someone later adds `impl Provider for ProviderInstance`, the compiler will reject both impls as conflicting. No comment warns of this constraint.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T72. `ModelInfo` missing cache pricing tiers — `model.rs`

`ModelInfo` has only `input_per_million` / `output_per_million`. Anthropic charges different rates for cache writes (1.25x input) and reads (0.1x input). Without cache pricing fields, fixing L6 (cost inaccuracy) would still compute cache tokens at the wrong rate.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T73. `BUILTIN_MODELS` missing short-form model aliases — `model.rs`

Anthropic publishes aliases like `claude-sonnet-4` -> `claude-sonnet-4-20250514`. Users who specify the alias get `resolve_model` returning `None`, yielding zero-cost reporting.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T74. `BUILTIN_MODELS` missing new models — `model.rs`

Models available as of the current knowledge cutoff that are absent: OpenAI `o3`, `gpt-4.1`, and Anthropic `claude-haiku-4`. Users of these models get `cost_usd: 0.0` without a config `[pricing]` override.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T75. `load_from_file` does not validate message ordering — `context.rs`

Deserialised contexts bypass all push-method invariants. A persisted file could contain two consecutive `Assistant` messages, misplaced `ToolResult` blocks, or an assistant-first sequence. The API would reject the malformed history with an opaque error rather than a clear validation message.

- **Severity:** Low — **Fix Risk:** Medium — **Effort:** Low

### T76. `ProviderError` missing `InvalidRequest` variant — `error.rs`

`chat_completions::validate_params` uses `ResponseParse` for client-side validation errors (e.g., tools + output_schema mutual exclusion). The root cause is the absence of an `InvalidRequest(String)` (or `ValidationFailed`) variant.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T77. `From<serde_json::Error>` maps all JSON errors to `ContextParse` — `error.rs`

Any `serde_json::Error` propagated via `?` in a `FlickError` context becomes `FlickError::ContextParse`, even when unrelated to context parsing. The variant name misleads callers handling JSON errors from non-context paths.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T78. `Message.content` missing `#[serde(default)]` — `context.rs`

A serialised message with the `content` key absent fails deserialisation with "missing field". Externally produced or hand-edited context files may omit it.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T82. No test for `ContextOverflow` during `runner::run` — `runner.rs`

`push_assistant` can return `ContextOverflow`. No test pre-loads a context near the 1024-message limit and verifies that `runner::run` propagates `FlickError::ContextOverflow`.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low

### T86. `load_config` helper swallows `ConfigError` in expect message — `tests/common/mod.rs`

`.expect("config should parse")` masks the actual `ConfigError` variant and message on test failure.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T87. Context persistence tests do not verify provider received full history — `tests/integration.rs`

`end_to_end_context_persistence` and `end_to_end_context_file_loading` verify `context.messages.len()` after the second turn but do not call `captured_params()` on the provider to confirm that the full message history was transmitted.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low

