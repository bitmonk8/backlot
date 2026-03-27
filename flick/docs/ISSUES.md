# Flick — Known Issues

Issues identified during review but deferred for later resolution.

---

## 1. No Anthropic prompt caching

**File:** `flick/src/provider/messages.rs`
**Category:** Enhancement (performance)
**Source:** rig integration testing (F-004)

Flick formats the system prompt as an array-of-content-blocks (which supports `cache_control`), but never attaches `cache_control: { type: "ephemeral" }` to any block. Every turn in a multi-turn reel session re-processes the full system prompt and tool definitions from scratch.

Impact observed in vault tests: bootstrap (Sonnet, 4 tool calls) takes 47s; record (Haiku, ~4 tool calls) takes 93s. The per-turn overhead of 8-13s is largely input re-processing that prompt caching would eliminate.

---

## 2. `validate_resolved_from_provider_info` adapter could be inlined

**File:** `flick/src/validation.rs`
**Category:** Simplification

Thin wrapper that unpacks `ProviderInfo` fields and forwards to `validate_resolved`. Called from one site. The caller could call `validate_resolved` directly.

---

## 3. `validate_assistant_content` could fold into `validate_message_structure`

**File:** `flick/src/context.rs`
**Category:** Simplification

`validate_assistant_content` iterates all messages a second time to check one condition (empty assistant content). Could be merged into the existing `validate_message_structure` loop.

---

## 4. FlickResult construction duplicated in runner

**File:** `flick/src/runner.rs`
**Category:** Simplification

Two-step and single-step paths both construct `FlickResult` with `UsageSummary` in near-identical fashion.

---

## 5. `_ = compat` dead parameter in validate_resolved

**File:** `flick/src/validation.rs`
**Category:** Simplification

`validate_resolved` accepts `Option<&CompatFlags>` that is immediately discarded. Reserved for future use but adds noise to call sites.

---

## 6. `CompatFlags` placement in provider_registry

**File:** `flick/src/provider_registry.rs`
**Category:** Separation of concerns

`CompatFlags` describes provider behavioral quirks consumed by validation and providers, not registry-specific. Could move to a shared types module.

---

## 7. `flick_dir()` and `home_dir()` in provider_registry

**File:** `flick/src/provider_registry.rs`
**Category:** Separation of concerns

General path utilities unrelated to provider credential management. Other modules needing the flick directory must import from provider_registry.

---

## 8. `validate_resolved` naming

**File:** `flick/src/validation.rs`
**Category:** Naming

`validate_resolved` is vague. A name like `validate_config_against_provider` would communicate what is validated and against what.

---

## 9. `platform.rs` module name is broad

**File:** `flick/src/platform.rs`
**Category:** Naming

Currently contains only one Windows ACL function. `permissions.rs` or `fs_permissions.rs` would be more precise.

---

## 10. `crypto.rs` `provider` parameter name

**File:** `flick/src/crypto.rs`
**Category:** Naming

The `provider` parameter in `encrypt`/`decrypt` serves as AAD (additional authenticated data). The name is domain-specific rather than describing its cryptographic role.

---

## 11. `validation.rs` missing branch coverage

**File:** `flick/src/validation.rs`
**Category:** Testing

Missing tests for: ChatCompletions temperature > 2.0, reasoning+output_schema allowed on ChatCompletions, budget_tokens skipped on ChatCompletions, happy path.

---

## 12. `crypto.rs` missing invalid hex test

**File:** `flick/src/crypto.rs`
**Category:** Testing

`decrypt` has an error path for `hex::decode` failure but no test covers it.

---

## 13. `platform.rs` has zero test coverage

**File:** `flick/src/platform.rs`
**Category:** Testing

`restrict_windows_permissions` has no tests. A smoke test on Windows would catch regressions.
