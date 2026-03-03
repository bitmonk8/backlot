# Flick — Open Issues

## Fix Later (21)

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

### L6. Cache token usage not accumulated in agent loop — `agent.rs:96-99`

`cache_creation_input_tokens` and `cache_read_input_tokens` are emitted per-request but not tracked in `RunSummary`. Cost computation uses only `input_tokens`/`output_tokens`. Cost will be inaccurate for cached Anthropic conversations.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Robustness

### L7. `ContentBlock` has no unknown-variant fallback — `context.rs:29-55`

Deserialization of an unknown `type` field (e.g., `{"type":"image"}`) produces a hard error. If a future provider or persisted context file contains an unfamiliar content block type, `Context::load_from_file` fails entirely.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Robustness

### L12. `shell_exec` bypasses resource restrictions without config-time warning — `tool.rs`

`shell_exec = true` nullifies all `[[resources]]` sandboxing. A user who configures both may have a false sense of security. No warning is emitted when `shell_exec` is enabled alongside resources.

- **Severity:** Medium — **Fix Risk:** Medium — **Effort:** Low
- **Category:** Security

### L13. No output size cap on shell/custom tool results — `tool.rs:424-459`

`shell_exec` and custom tool output collected via `cmd.output()` has no size limit. A runaway command producing gigabytes causes OOM.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Robustness

### L14. Windows `escape_for_cmd` trailing backslash can escape closing quote — `tool.rs:493-507`

Value `C:\Users\test\` becomes `"C:\Users\test\"` — the `\"` escapes the closing quote in programs using `CommandLineToArgvW`. Can cause subtle argument-parsing bugs on Windows.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Security

### L12. No test for `build_request` / dry-run path — `tests/`

The `--dry-run` code path that calls `build_request` is completely untested at the integration level. `MockProvider::build_request` returns `Ok(json!({}))` always.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low
- **Category:** Test Coverage

### L17. No test for `ProviderError::RateLimited` propagation — `tests/`

No test verifies that a provider returning `RateLimited` propagates correctly through the agent loop as `FlickError::Provider(ProviderError::RateLimited { .. })`.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low
- **Category:** Test Coverage

### L18. No test for concurrent tool execution semantics — `tests/`

`join_all` runs all pending tool calls concurrently. No test verifies that concurrent execution handles mixed success/failure of concurrent calls correctly.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Medium
- **Category:** Test Coverage

### L19. `setup` errors always emit JSON to stderr, even interactive — `main.rs:79`

`cmd_setup` hardcodes `raw=false`, so any setup error emits a JSON-lines object to stderr. A terminal user running `flick setup anthropic` sees `{"type":"error","message":"...","code":"...","fatal":true}` instead of a human-readable message. The `run` command correctly honours `--raw`; setup does not.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial
- **Category:** UX

### L20. Unix key-write failure leaves corrupted `.secret_key` — `credential.rs:92-111`

On the Unix code path in `load_or_create_secret_key`, if `file.write_all(hex_key.as_bytes()).await` fails the error propagates via `?` without deleting the partially-written file. Subsequent `load_secret_key` calls fail with `InvalidFormat`; `load_or_create_secret_key` retries `create_new` which hits `AlreadyExists` and re-loads the corrupt file. The Windows path correctly deletes on write failure.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial
- **Category:** Bug

### L21. `output_schema` silently ignored by Messages provider — `provider/messages.rs:96-101`

When `params.output_schema` is `Some`, the Messages provider discards it without emitting a warning. A user who configures `[output_schema]` and targets the Anthropic Messages API receives unstructured output with no diagnostic. The Chat Completions provider honours the field via `response_format`.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial
- **Category:** UX

### L22. Empty thinking signature stored in context, breaks next API call — `agent.rs`

When the provider returns thinking content with an empty signature (e.g., Chat Completions provider, or a malformed Anthropic response), a `ContentBlock::Thinking { signature: "" }` is pushed to context. When that context is later replayed to the Anthropic Messages API in a multi-turn session, the empty signature violates the API contract and produces a hard error on the next iteration.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Bug

### L23. Reasoning + `output_schema` mutual exclusion not enforced — `agent.rs:216-226`, `config.rs`

`build_params` strips temperature when reasoning is active but does not strip `output_schema`. The Anthropic API rejects requests combining extended thinking with structured output. No validation guards this combination at config load or at request build time; the error surfaces as an opaque API 400.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Low
- **Category:** Bug

### L24. Custom tool `command`/`executable` bypasses resource sandboxing — `tool.rs:566-592`

Custom tools with `command` or `executable` never call `check_access`. A user who disables `shell_exec` and configures `[[resources]]` can still get unrestricted filesystem access via any `[[tools.custom]]` entry. No warning is emitted.

- **Severity:** Medium — **Fix Risk:** Medium — **Effort:** Low (documentation/warning path)
- **Category:** Security

### L25. TOCTOU between `check_access` and `write_file` — `tool.rs:295, 311-315`

For non-existent destination paths, `check_access` resolves an ancestor and appends non-existent suffix components. Between that check and the subsequent `create_dir_all` + `write`, an attacker with write access to the resource directory can plant a symlink at any non-existent intermediate component, redirecting the write outside the resource boundary.

- **Severity:** Medium — **Fix Risk:** Medium — **Effort:** Medium
- **Category:** Security

### L26. `write_file` has no content size cap — `tool.rs:307-316`

`read_file` has a 10 MiB cap (`READ_FILE_MAX_BYTES`). `write_file` accepts content of unlimited size. An LLM can instruct writing a multi-gigabyte string, exhausting the filesystem.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial
- **Category:** Robustness

---

## Fix When Touched (72)

### T1. `read_stdin` accepts unlimited input size — `main.rs:246-248`

`read_to_string` reads all of stdin with no size cap. A multi-gigabyte pipe causes OOM before the LLM API rejects it.

- **Severity:** Medium — **Fix Risk:** Low — **Effort:** Trivial

### T2. `cmd_setup_core` does not validate provider name length — `main.rs:214-225`

Extremely long provider name passes validation but fails at OS level with an unhelpful error. Add `|| provider_name.len() > 255`.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T3. `cmd_setup_core` does not validate API key content — `main.rs:228-233`

No length cap or control character check on the API key value.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T4. Empty custom tool description accepted — `config.rs`

`description = ""` passes validation. Functionally useless to the model.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T5. Resource paths not validated at config load time — `config.rs:135-146`

Empty paths, non-existent paths, and paths with unusual components accepted without validation. Enforcement deferred to tool module.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T6. `encrypt` does not zeroize intermediate plaintext bytes — `credential.rs:342-355`

The `plaintext` parameter is a `&str` from the caller. Same root cause as L3 from the caller side.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T7. No provider name validation in `CredentialStore::get`/`set` — `credential.rs:30,40`

Public API accepts any `&str` including TOML-special characters. Caller validates, but the store itself does not enforce invariants.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T8. `encrypt` error uses wrong variant name — `credential.rs:349`

`InvalidFormat("encryption failed")` — semantically wrong for an encryption operation. The error path is practically unreachable.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T10. `build_body` does not enforce temperature + thinking mutual exclusion — `messages.rs:42-101`

Anthropic API rejects requests with both `temperature` and thinking enabled. `build_body` does not guard; caller enforcement exists but is not defensive.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T11. Thinking budget silently becomes 0 when `max_tokens` < 2 — `messages.rs:87-93`

User requests reasoning but `max_tokens` is too small. Thinking is silently disabled with no warning.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T14. `convert_message` drops Thinking blocks silently for Chat Completions — `chat_completions.rs:244-253`

Thinking-only messages produce empty `"content": ""`. OpenAI accepts this but it is noise in the context window.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T17. Redundant `content-type` header — `chat_completions.rs:139`

`.header("content-type", "application/json")` is redundant with `.json(&body)` which sets it automatically.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T23. No error event emitted for iteration limit — `agent.rs:187-188`

Function returns `Err(FlickError::IterationLimit(25))` without emitting any `StreamEvent::Error`. Event stream just stops.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T24. `push_assistant` accepts empty content vec — `context.rs:75-84`

Caller guards against this, but the method itself does not validate. Empty assistant message would violate API constraints.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T25. `FlickError::code()` couples to `ProviderError` internals — `error.rs:33-54`

`FlickError::code()` must know about all `ProviderError` variants rather than delegating to `ProviderError::code()`.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T26. `push_tool_results` uses `ToolError::ExecutionFailed` for programming errors — `context.rs:86-109`

Non-ToolResult blocks and empty results return `ExecutionFailed`, which semantically means tool execution failed, not caller-API misuse.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T27. TOCTOU race in `read_file` between size check and read — `tool.rs:296-304`

File could grow between `metadata.len()` check and `read_to_string`. Read first, then check size.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T28. Unknown template placeholder left verbatim in shell command — `tool.rs:551-554`

`{{key}}` with no matching parameter preserved as literal text. Should be an error.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T29. `first_allowed_dir` uses synchronous `is_dir()` in async context — `tool.rs:463-469`

Blocks tokio runtime thread. Negligible for typical configs with few resources.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Low

### T30. `run_shell` does not explicitly pipe stdout/stderr — `tool.rs:45-56`

`cmd.output()` implicitly pipes, but explicit `piped()` would be consistent with `run_executable`.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T31. Executable path not validated before spawn — `tool.rs:580-586`

Invalid executable path produces generic OS error instead of descriptive tool error.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T32. `ReadWrite` subsuming `Read` in access check undocumented — `tool.rs:411-414`

Match pattern `(Read, ReadWrite)` is correct but deserves a comment explaining the subsumption.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T33. Iteration limit test doesn't verify context state — `tests/agent.rs:108-137`

`run_iteration_limit_exhaustion` does not verify that partial work (context messages) is correctly preserved before the error.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Low

### T34. No integration test for template values needing shell quoting — `tests/integration.rs`

Unit tests cover Windows quoting, but no integration test exercises template substitution with values containing spaces or special characters.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Low

### T35. `MockProvider::captured_params()` is a destructive read — `tests/common/mod.rs:59-62`

`std::mem::take` means second call returns empty vec. Subtle footgun if reused.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T36. Thinking blocks test does not verify `Done` event — `tests/integration.rs:167-221`

`end_to_end_thinking_blocks` does not check Done event content (iteration count, cost).

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T37. JSON lines output test missing `Usage` event — `tests/integration.rs:281-322`

Mock provider emits no `Usage` event. Does not test the common 3-line output (text + usage + done).

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T38. Non-fatal error test doesn't verify Done iteration count — `tests/agent.rs`

`run_nonfatal_warning_emitted` does not confirm `iterations == 1` in the Done event.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T40. Whitespace-only stdin produces misleading `no_query` error — `main.rs:248, 173`

`read_stdin` trims the input to an empty string, which hits the `NoQuery` path. The error message "use --query or pipe to stdin" is misleading when the user *did* pipe to stdin (but sent only whitespace).

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T41. Provider name allows dot-prefixed names like `.hidden` — `main.rs:215-217`

Validation rejects exactly `"."` and `".."` but permits `.hidden`, `...`, `..foo`, etc. Dot-prefixed credential files are invisible by default on Unix.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T42. `override_reasoning` uses `take()` instead of `replace()` — `config.rs:218`

`override_model_name` uses `std::mem::replace` for transactional rollback; `override_reasoning` uses `take()` then assigns. If `validate()` ever panics, the field is left as `Some(new_value)` and `old` is dropped.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T43. Zero cost reported without warning for unknown models — `config.rs:363-366`

When neither a `[pricing]` config section nor a builtin registry entry exists for the model, `compute_cost` returns `0.0`. The `Done` event reports `cost_usd: 0.0`, which misleads the caller into thinking the call was free.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Low

### T44. Custom tool `parameters` not validated as JSON Schema — `config.rs:126`

`CustomToolConfig.parameters` accepts any `serde_json::Value` (string, number, array). An invalid schema passes config validation and is forwarded to the model; the API rejects it at request time with an opaque error.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T45. Empty `--query ""` not rejected before config/credential I/O — `main.rs:102-155`

When `--query ""` is passed explicitly, `cmd_run` loads config, decrypts credentials, and optionally reads a context file before `cmd_run_core` rejects it with `NoQuery`. The I/O is wasted and credential errors obscure the real issue.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T46. No test for `--raw --dry-run` combined warning path — `main.rs:142-146`

The `(true, true)` arm emits a warning to stderr and falls through to `DryRun`. No test covers this code path.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T47. `decrypt` internals not zeroized — `credential.rs:361-374`

Inside `decrypt`, the `combined` Vec (nonce + ciphertext) and the `plaintext` Vec returned by `cipher.decrypt()` are plain allocations. The plaintext bytes survive in heap memory after drop.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T48. Temp credentials file not cleaned up on `rename` failure — `credential.rs:165-179`

If `tokio::fs::rename` fails (e.g., destination locked on Windows), the `.tmp` file containing all credentials is left on disk. It will be overwritten on the next `set()` call so there is no data-loss, but it is a robustness gap.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T49. Poly1305 authentication tag size 16 is a magic number — `credential.rs:363`

The minimum-length check `combined.len() < NONCE_LEN + 16` uses the literal `16` (Poly1305 tag length) without a named constant.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T50. No test for `get()` when no secret key file exists — `credential.rs` (tests)

All `get()` tests create a key via `set()` first. There is no test verifying that `get()` before any `set()` returns `CredentialError::NoSecretKey`.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T51. Secret key file not `fsync`'d before returning — `credential.rs:103-105, 125`

Neither the Unix nor Windows path calls `sync_all()` after writing the key file. A power failure between `write_all` and OS flush leaves the file empty or truncated.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T52. Redundant `content-type` header in Messages provider — `messages.rs:118`

`.header("content-type", "application/json")` is redundant because `.json(&body)` sets it automatically.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T54. System prompt serialised as plain string, blocking prompt caching — `messages.rs:53-55`

`body["system"] = json!(system)` produces a JSON string. The Anthropic API also accepts `system` as an array of content blocks, which is required to attach `cache_control` headers for prompt caching.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T55. No `tool_choice` configuration surface for Messages provider — `messages.rs:65-84`

The Messages provider always omits `tool_choice`, relying on the Anthropic default of `auto`. There is no way to force `{"type": "any"}` or a specific tool.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T56. User-role `ToolUse` blocks silently dropped in `convert_message` — `chat_completions.rs:212-253`

The `has_tool_use` branch is gated on `role == "assistant"`. A `Message` with `Role::User` containing `ToolUse` blocks falls through to the text-only path, silently dropping the blocks.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T57. No `"strict": true` on tool function definitions — `chat_completions.rs:256-276`

OpenAI supports `"strict": true` on function tool definitions for schema-enforced argument generation. The provider sets `"strict": true` for `response_format` but not for tools, creating an inconsistency.

- **Severity:** Low — **Fix Risk:** Medium — **Effort:** Low

### T59. Tool result `is_error` double-prefixes "Error:" in Chat Completions — `chat_completions.rs:195-196`

Error content is wrapped as `format!("Error: {content}")`. Tool implementations in `tool.rs` already produce messages like `"Error: file not found"`, resulting in `"Error: Error: file not found"` in the API payload.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T60. `validate_params` does not reject empty messages array — `chat_completions.rs:116-123`

An empty `params.messages` would be serialised and rejected by the OpenAI API with an opaque 400 error.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T63. HTTP 408 (Request Timeout) not classified as retryable — `http.rs`

`handle_http_error` maps 408 to `ProviderError::Api`, which `classify_for_retry` treats as non-retryable. RFC 7231 explicitly permits retry on 408.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T65. Blanket/manual `DynProvider` coherence trap undocumented — `provider.rs:68-83, 140-154`

`ProviderInstance` has a manual `DynProvider` impl, coexisting with the blanket `impl<T: Provider> DynProvider for T`. If someone later adds `impl Provider for ProviderInstance`, the compiler will reject both impls as conflicting. No comment warns of this constraint.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T67. Tool panic in `join_all` aborts loop without emitting error event — `agent.rs:156-167`

If a tool execution future panics (e.g., a programming error in a custom-tool handler), `join_all` propagates the panic, unwinding through the agent loop. No `StreamEvent::Error` is emitted before the process exits.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T68. `write_file` non-atomic — partial writes corrupt existing files on crash — `tool.rs:315`

`tokio::fs::write` truncates and rewrites the target path directly. A process crash mid-write destroys the original content. The credential store uses atomic rename-based writes; `write_file` does not.

- **Severity:** Low — **Fix Risk:** Medium — **Effort:** Low

### T69. `list_directory`: single bad entry aborts entire listing — `tool.rs:330`

`entry.file_type().await?` propagates an error for one broken directory entry (e.g., a dangling symlink in a TOCTOU window), aborting the entire listing. The remaining valid entries are lost.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T71. `RunSummary` missing cache token count fields — `event.rs:57-63`

`RunSummary` (emitted in the `Done` event) carries only `input_tokens` and `output_tokens`. The per-request `StreamEvent::Usage` events include cache token counts, but these are not surfaced in the final summary.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Trivial

### T72. `ModelInfo` missing cache pricing tiers — `model.rs:14-19`

`ModelInfo` has only `input_per_million` / `output_per_million`. Anthropic charges different rates for cache writes (1.25× input) and reads (0.1× input). Without cache pricing fields, fixing L9 (cost inaccuracy) would still compute cache tokens at the wrong rate.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T73. `BUILTIN_MODELS` missing short-form model aliases — `model.rs:45-86`

Anthropic publishes aliases like `claude-sonnet-4` → `claude-sonnet-4-20250514`. Users who specify the alias get `resolve_model` returning `None`, yielding zero-cost reporting in `Done` events.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T74. `BUILTIN_MODELS` missing new models — `model.rs:45-86`

Models available as of the current knowledge cutoff that are absent: OpenAI `o3`, `gpt-4.1`, and Anthropic `claude-haiku-4`. Users of these models get `cost_usd: 0.0` in `Done` events without a config `[pricing]` override.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T75. `load_from_file` does not validate message ordering — `context.rs:58-62`

Deserialised contexts bypass all push-method invariants. A persisted file could contain two consecutive `Assistant` messages, misplaced `ToolResult` blocks, or an assistant-first sequence. The API would reject the malformed history with an opaque error rather than a clear validation message.

- **Severity:** Low — **Fix Risk:** Medium — **Effort:** Low

### T76. `ProviderError` missing `InvalidRequest` variant — `error.rs:78-100`

`chat_completions::validate_params` uses `ResponseParse` for client-side validation errors (e.g., tools + output_schema mutual exclusion). The root cause is the absence of an `InvalidRequest(String)` (or `ValidationFailed`) variant.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T77. `From<serde_json::Error>` maps all JSON errors to `ContextParse` — `error.rs:27`

Any `serde_json::Error` propagated via `?` in a `FlickError` context becomes `FlickError::ContextParse`, even when unrelated to context parsing. The variant name misleads callers handling JSON errors from non-context paths.

- **Severity:** Low — **Fix Risk:** Low — **Effort:** Low

### T78. `Message.content` missing `#[serde(default)]` — `context.rs:16-20`

A serialised message with the `content` key absent fails deserialisation with "missing field". Externally produced or hand-edited context files may omit it.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T81. `Done` event `usage` fields unverified in JSON-lines output test — `tests/integration.rs:315-321`

`end_to_end_json_lines_output` asserts `parsed["type"] == "done"` but does not check `usage.input_tokens`, `usage.output_tokens`, `usage.iterations`, or `usage.cost_usd`.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T82. No test for `ContextOverflow` during agent loop execution — `agent.rs:146, 184`

`push_assistant` and `push_tool_results` can return `ContextOverflow`. No test pre-loads a context near the 1024-message limit and verifies that `agent::run` propagates `FlickError::ContextOverflow`.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low

### T83. Fragile temperature assertion depends on implicit `reasoning=None` — `tests/agent.rs:627-637`

`run_forwards_correct_params_to_provider` asserts `temperature == Some(0.5)` without first asserting `reasoning == None`. If reasoning were ever enabled in the stub config, `build_params` would strip temperature and the test would fail for the wrong reason.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T84. No raw-mode integration test with tool calls — `tests/integration.rs`

`end_to_end_raw_output` exercises text-only output. No test verifies that `RawEmitter` silently drops `ToolCall` and `ToolResult` events.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Low

### T85. Thinking-before-text content ordering not verified — `tests/integration.rs:211-220`

`end_to_end_thinking_blocks` verifies the existence and content of `Thinking` and `Text` blocks but not that `Thinking` precedes `Text`. The agent loop pushes thinking first; a regression reversing this would violate the Anthropic API contract but pass the test.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T86. `load_config` helper swallows `ConfigError` in expect message — `tests/common/mod.rs:123-126`

`.expect("config should parse")` masks the actual `ConfigError` variant and message on test failure.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

### T87. Context persistence tests do not verify provider received full history — `tests/integration.rs:370-476`

`end_to_end_context_persistence` and `end_to_end_context_file_loading` verify `context.messages.len()` after the second turn but do not call `captured_params()` on the provider to confirm that the full message history was transmitted.

- **Severity:** Medium — **Fix Risk:** None — **Effort:** Low

### T88. No test with non-zero cache token values — `agent.rs:96`

All test `StreamEvent::Usage` events set `cache_creation_input_tokens: 0` and `cache_read_input_tokens: 0`. The `..` destructure silently discards cache fields. No test exercises the path where cache tokens are non-zero.

- **Severity:** Low — **Fix Risk:** None — **Effort:** Trivial

---

## Post-SSE Removal Simplifications

SSE/streaming support was removed (src/provider/sse.rs deleted, non-streaming `ModelResponse` path only). All vestiges fixed.

### ~~S1. `StreamEvent` → `Event` rename~~ — DONE

### ~~S2. Comment/doc vestiges of streaming~~ — DONE

### ~~S3. Vestigial `stream` assertions in provider tests~~ — DONE

### S4. `futures-util` dependency used only for `join_all` — `Cargo.toml:20`, `agent.rs:1`

The entire `futures-util` crate (with `alloc` feature) is pulled in for a single `join_all` call in `execute_tools`. Alternatives:
- Inline a 10-line `join_all` equivalent using `Pin<Box<dyn Future>>` and manual polling
- Accept the dependency (it is lightweight and well-maintained)

Not a clear win — document for awareness. The dependency is justified if concurrent tool execution remains important.

- **Fix Risk:** Low — **Effort:** Low

### ~~S5. `ProviderInstance::inner()` vtable indirection~~ — DONE

### ~~S6. Stale references in existing findings~~ — DONE

