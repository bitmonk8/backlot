# Flick — Backlog

3 items in 1 active cluster, ordered by value (highest first).

Original IDs (L*n*, T*n*) preserved for traceability. Severity markers: **M** = medium, **L** = low.

---

## 1. Test Coverage Gaps (3 items)

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
