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

## 3. `CompatFlags` placement in provider_registry

**File:** `flick/src/provider_registry.rs`
**Category:** Separation of concerns

`CompatFlags` describes provider behavioral quirks consumed by validation and providers, not registry-specific. Could move to a shared types module.

---

## 4. `flick_dir()` and `home_dir()` in provider_registry

**File:** `flick/src/provider_registry.rs`
**Category:** Separation of concerns

General path utilities unrelated to provider credential management. Other modules needing the flick directory must import from provider_registry.

---

## 5. `validate_resolved` naming

**File:** `flick/src/validation.rs`
**Category:** Naming

`validate_resolved` is vague. A name like `validate_config_against_provider` would communicate what is validated and against what.

---

## 6. `platform.rs` module name is broad

**File:** `flick/src/platform.rs`
**Category:** Naming

Currently contains only one Windows ACL function. `permissions.rs` or `fs_permissions.rs` would be more precise.

---

## 7. `crypto.rs` `provider` parameter name

**File:** `flick/src/crypto.rs`
**Category:** Naming

The `provider` parameter in `encrypt`/`decrypt` serves as AAD (additional authenticated data). The name is domain-specific rather than describing its cryptographic role.

---

## 8. `validation.rs` missing branch coverage

**File:** `flick/src/validation.rs`
**Category:** Testing

Missing tests for: ChatCompletions temperature > 2.0, reasoning+output_schema allowed on ChatCompletions, budget_tokens skipped on ChatCompletions, happy path.

---

## 9. `crypto.rs` missing invalid hex test

**File:** `flick/src/crypto.rs`
**Category:** Testing

`decrypt` has an error path for `hex::decode` failure but no test covers it.

---

## 10. `platform.rs` has zero test coverage

**File:** `flick/src/platform.rs`
**Category:** Testing

`restrict_windows_permissions` has no tests. A smoke test on Windows would catch regressions.

---

## 11. FlickResult construction duplicated in runner

**File:** `flick/src/runner.rs`
**Category:** Simplification

Two-step and single-step paths both construct `FlickResult` with `UsageSummary` in near-identical fashion.

---

## 12. `_ = compat` dead parameter in validate_resolved

**File:** `flick/src/validation.rs`
**Category:** Simplification

`validate_resolved` accepts `Option<&CompatFlags>` that is immediately discarded. Reserved for future use but adds noise to call sites.

---

## 13. `input_tokens` semantics differ between providers

**File:** `flick/src/provider/chat_completions.rs`, `flick/src/provider/messages.rs`
**Category:** Correctness

Chat Completions reports non-cached input tokens (prompt_tokens minus cached_tokens). Messages API reports total input tokens as-is. Both are correct for their respective APIs and cost computation works correctly, but `UsageSummary.input_tokens` has different semantics across providers for reporting purposes.
