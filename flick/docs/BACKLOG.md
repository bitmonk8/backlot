# Flick — Backlog

7 items in 2 active clusters, ordered by value (highest first).

Original IDs (L*n*, T*n*) preserved for traceability. Severity markers: **M** = medium, **L** = low.

---

## 1. Provider — Messages API & Architecture (4 items)

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

## 2. Test Coverage Gaps (3 items)

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
