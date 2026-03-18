# Flick — Known Issues

Issues identified during review but deferred for later resolution.

---

## 1. Double-counting cached tokens for Chat Completions providers

**File:** `flick/src/config.rs` (compute_cost), `flick/src/provider/chat_completions.rs`
**Category:** Correctness

OpenAI-compatible providers include cached tokens within `prompt_tokens` (i.e., `input_tokens` already contains the cached subset). `compute_cost` then charges cached tokens at both the input rate and the cache-read rate. No impact today because no Chat Completions model has `cache_creation_per_million` or `cache_read_per_million` set, but will over-report cost if a user configures cache pricing for an OpenAI-compatible model.

**Fix:** Either subtract cache tokens from `input_tokens` in the Chat Completions response parser, or make `compute_cost` provider-aware.

---

## 2. `compute_cost` is a method on `RequestConfig` but uses no `self` fields

**File:** `flick/src/config.rs:288-310`
**Category:** Separation of concerns

`compute_cost` takes `&self` but reads only `ModelInfo` fields and token count arguments. Should be a method on `ModelInfo` or a free function. Moving it is a refactor that touches every caller and test.

---

## 3. Nested `mul_add` in `compute_cost` is harder to read than a simple sum

**File:** `flick/src/config.rs:297-310`
**Category:** Simplification

The nested `mul_add` chain could be replaced with a plain `a*b + c*d + ...` expression. Not a hot path; readability matters more than FMA precision here.

---

## 4. Repeated pricing validation blocks in `validate_model_entry`

**File:** `flick/src/model_registry.rs:138-169`
**Category:** Simplification

Four nearly identical blocks validate pricing fields with the same `!v.is_finite() || v < 0.0` check. A helper function would reduce duplication.

---

## 5. Base URL validation allows degenerate URLs

**File:** `flick/src/provider_registry.rs:122-126`
**Category:** Testing

`set()` checks that `base_url` starts with `http://` or `https://` but does not validate the URL is well-formed beyond that. Degenerate values like `"https://"` (no host) pass validation but would fail at reqwest request time. Stricter parsing (e.g., `url::Url::parse`) would catch these earlier.

---

## 6. No test coverage for corrupt secret key file

**File:** `flick/src/provider_registry.rs:163-181`
**Category:** Testing

`load_secret_key` has error paths for invalid hex and wrong-length keys, but no test covers reading a corrupt `.secret_key` file. Could be tested by writing invalid content to the key file path before calling `get()`.

---

## 7. `#[serde(untagged)]` on `ContentBlock::Unknown` silently swallows malformed known types

**File:** `flick/src/context.rs:29-59`
**Category:** Correctness

If a known type (e.g. `{"type":"text","text":42}`) has the right `type` tag but invalid field types, serde fails to match the tagged variant and silently falls through to `Unknown(Value)`. Fixing requires a custom `Deserialize` impl — high effort, low practical likelihood since provider responses are well-typed.

---

## 8. `push_*` methods don't enforce message alternation

**File:** `flick/src/context.rs:109-173`
**Category:** Correctness

`push_user_text`, `push_assistant`, and `push_tool_results` don't check `self.messages.last()` to prevent consecutive same-role messages. `validate_message_order` only runs on `load_from_file`. Existing callers enforce correct ordering, but the methods themselves are not defensive.

---

## 9. `#[serde(default)]` allows empty-content assistant messages to load

**File:** `flick/src/context.rs:15-20`
**Category:** Correctness

A serialized assistant message with missing `content` key deserializes to `content: vec![]`, which `push_assistant` would reject but `load_from_file` accepts. Intentionally lenient on load, but inconsistent.

---

## 10. `validate_message_order` doesn't check `ToolUse` in user messages

**File:** `flick/src/context.rs:84-104`
**Category:** Correctness

Checks `ToolResult` blocks are only in user messages but doesn't check the symmetric constraint: `ToolUse` blocks should only appear in assistant messages. Only affects hand-edited context files.

---

## 11. Missing error code tests for `InvalidAssistantContent` and `InvalidMessageOrder`

**File:** `flick/src/error.rs:57-58`
**Category:** Testing

The `code()` method maps these new variants to string codes, but no test verifies the mapping.

---

## 12. Flaky test `history::tests::record_with_resume_hash` on ubuntu-latest CI

**File:** `flick/src/history.rs:183-208`
**Category:** Testing

`record_with_resume_hash` intermittently fails on ubuntu-latest CI runners. The test writes to a tempdir via `tokio::fs` async append, then reads back and asserts the line contains `"resume_hash":"somehash"`. Passes consistently on macOS, Windows, and local ubuntu. Likely a filesystem timing issue with async I/O on CI ephemeral runners. Observed 2026-03-18 (CI run 23239580696).

**Fix:** Either add an explicit `file.flush().await` / `file.shutdown().await` before reading back, or switch the test to synchronous I/O since it only writes one line.
