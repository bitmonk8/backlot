# Flick — Known Issues

Issues identified during review but deferred for later resolution.

---

## 1. `validate_resolved_from_provider_info` adapter could be inlined

**File:** `flick/src/validation.rs`
**Category:** Simplification

Thin wrapper that unpacks `ProviderInfo` fields and forwards to `validate_resolved`. Called from one site. The caller could call `validate_resolved` directly.

---

## 2. `validate_assistant_content` could fold into `validate_message_structure`

**File:** `flick/src/context.rs`
**Category:** Simplification

`validate_assistant_content` iterates all messages a second time to check one condition (empty assistant content). Could be merged into the existing `validate_message_structure` loop.

---

## 3. FlickResult construction duplicated in runner

**File:** `flick/src/runner.rs`
**Category:** Simplification

Two-step and single-step paths both construct `FlickResult` with `UsageSummary` in near-identical fashion.

---

## 4. `_ = compat` dead parameter in validate_resolved

**File:** `flick/src/validation.rs`
**Category:** Simplification

`validate_resolved` accepts `Option<&CompatFlags>` that is immediately discarded. Reserved for future use but adds noise to call sites.

---

## 5. `CompatFlags` placement in provider_registry

**File:** `flick/src/provider_registry.rs`
**Category:** Separation of concerns

`CompatFlags` describes provider behavioral quirks consumed by validation and providers, not registry-specific. Could move to a shared types module.

---

## 6. `flick_dir()` and `home_dir()` in provider_registry

**File:** `flick/src/provider_registry.rs`
**Category:** Separation of concerns

General path utilities unrelated to provider credential management. Other modules needing the flick directory must import from provider_registry.

---

## 7. `validate_resolved` naming

**File:** `flick/src/validation.rs`
**Category:** Naming

`validate_resolved` is vague. A name like `validate_config_against_provider` would communicate what is validated and against what.

---

## 8. `platform.rs` module name is broad

**File:** `flick/src/platform.rs`
**Category:** Naming

Currently contains only one Windows ACL function. `permissions.rs` or `fs_permissions.rs` would be more precise.

---

## 9. `crypto.rs` `provider` parameter name

**File:** `flick/src/crypto.rs`
**Category:** Naming

The `provider` parameter in `encrypt`/`decrypt` serves as AAD (additional authenticated data). The name is domain-specific rather than describing its cryptographic role.

---

## 10. `validation.rs` missing branch coverage

**File:** `flick/src/validation.rs`
**Category:** Testing

Missing tests for: ChatCompletions temperature > 2.0, reasoning+output_schema allowed on ChatCompletions, budget_tokens skipped on ChatCompletions, happy path.

---

## 11. `crypto.rs` missing invalid hex test

**File:** `flick/src/crypto.rs`
**Category:** Testing

`decrypt` has an error path for `hex::decode` failure but no test covers it.

---

## 12. `platform.rs` has zero test coverage

**File:** `flick/src/platform.rs`
**Category:** Testing

`restrict_windows_permissions` has no tests. A smoke test on Windows would catch regressions.

---

## 13. `CacheRetention::Long` TTL format may not match API

**File:** `flick/src/provider/messages.rs`
**Category:** Correctness

`CacheRetention::Long` emits `"ttl": "1h"` (string). Anthropic API documentation has shown both string and integer formats at different times. Verify against the current API whether `"1h"` or `3600` (integer seconds) is expected.

---

## 14. `CacheRetention` naming

**File:** `flick/src/config.rs`
**Category:** Naming

`CacheRetention` conflates "whether to cache" (the `None` variant disables injection entirely) with "how long to cache" (Short vs Long). A name like `CachePolicy` or `CacheMode` would cover both aspects more accurately.

---

## 15. Cache control test coverage gaps

**Files:** `flick/src/provider/chat_completions.rs`, `flick/src/config.rs`, `flick/src/runner.rs`
**Category:** Testing

Missing tests: (a) Chat Completions negative test asserting no `cache_control` in output, (b) `set_cache_retention` setter, (c) builder `cache_retention()` method, (d) `#[serde(skip)]` interaction with `deny_unknown_fields`, (e) `build_params` threading of cache_retention.
