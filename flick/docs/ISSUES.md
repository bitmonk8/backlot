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
