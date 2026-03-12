# Known Issues

## 1. `run_structured` ToolCallsPending branch is untested

`src/agent/flick.rs` — `run_structured` bails if the model returns `ToolCallsPending` (hallucinated tool calls in a structured-only context). No test exercises this branch. A `SingleShotProvider` returning `ToolCallsPending` status would cover it.

## 2. `FlickAgent::new()` error paths untested

`src/agent/flick.rs` — `FlickAgent::new()` can fail in two ways: `build_model_registry()` and `ProviderRegistry::load_default()`. Neither error path is tested — `with_injected` bypasses both. These are thin wrappers with straightforward error mapping, so the risk is low. Consider adding a `new_with_registries()` constructor or accepting an optional `ProviderRegistry` for testability if these paths grow more complex.

## 3. Config JSON round-trip in `build_config`

`src/agent/config_gen.rs` — `build_config` constructs a `serde_json::Value`, serializes to string, then passes to `RequestConfig::from_str` which re-parses. If flick exposes `from_value` or a builder API, the round-trip is eliminable. Check next time flick is updated.

## 4. Missing wire-type edge-case tests

`src/agent/config_gen.rs` — Several conversion error paths lack test coverage:
- `VerificationWire` with `outcome: "fail"` (both with and without `reason`)
- `parse_model_name` with invalid input (e.g., `"gpt4"`)
- `TaskOutcomeWire` with invalid outcome (e.g., `"partial"`)
- `DetectedStepWire` conversion: default timeout (300) when `timeout` is `None`
- `SubtaskWire` with invalid magnitude (e.g., `"huge"`)

## 5. `run_with_tools` resume timeout untested

`src/agent/flick.rs` — The timeout test only covers the initial `client.run()`. No test covers timeout during `client.resume()` in the tool loop. A `SlowProvider` that responds quickly on first call (with tool calls) but slowly on resume would cover this.

## 6. Timeout/error-mapping pattern duplication

`src/agent/flick.rs` — The `tokio::time::timeout(...).await.map_err(...)` pattern appears three times with near-identical structure. A small `timed_call` helper would deduplicate. Low urgency — cosmetic.

## 7. `model_key()` and `default_max_tokens()` placement

`src/agent/config_gen.rs` — Both functions encode model-tier policy (tier → registry key, tier → token budget) but live in config_gen.rs, whose stated purpose is "in-memory config, wire format types, output schemas." Their primary consumer is `build_model_registry()` in flick.rs. Move them to flick.rs or a shared module. **Category: Placement.**

## 8. `extract_text` mutable loop

`src/agent/flick.rs` — `extract_text` iterates all content blocks with a mutable `last_text` variable. `iter().rev().find_map(...)` is more direct. **Category: Simplification.**
