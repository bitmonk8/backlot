# Known Issues

## Non-critical issues

### 1. `ReelAgent::new()` error paths untested

`src/agent/reel_adapter.rs` — `ReelAgent::new()` can fail in two ways: `build_model_registry()` and `ProviderRegistry::load_default()`. Neither error path is tested. These are thin wrappers with straightforward error mapping, so the risk is low. **Category: Testing.**

### 2. Missing wire-type edge-case tests

`src/agent/wire.rs` — Several conversion error paths lack test coverage:
- `VerificationWire` with `outcome: "fail"` (both with and without `reason`)
- `parse_model_name` with invalid input (e.g., `"gpt4"`)
- `TaskOutcomeWire` with invalid outcome (e.g., `"partial"`)
- `DetectedStepWire` conversion: default timeout (300) when `timeout` is `None`
- `SubtaskWire` with invalid magnitude (e.g., `"huge"`)

**Category: Testing.**

### 3. `lot` dependency uses local path override

`Cargo.toml` — `lot = { path = "../lot" }` is a local dev override. Must revert to a pinned git rev before merge. Blocked on committing the lot policy.rs changes to the lot repo first. Applies to both epic and reel. **Category: Correctness.**

### 4. Hardcoded tier array in `build_model_registry`

`src/agent/reel_adapter.rs` — Iterates `[Model::Haiku, Model::Sonnet, Model::Opus]`. If `Model` gains variants, this silently becomes incomplete. Add `Model::ALL` or use exhaustive matching. **Category: Fragility.**

### 5. Redundant error wrapping on provider registry load

`src/agent/reel_adapter.rs` — `.map_err(|e| anyhow!(...))` on `ProviderRegistry::load_default()` adds no information beyond the original error. Use `anyhow::Context` or propagate directly. **Category: Simplification.**

### 6. `run_request` untested and adapter lost testability seam

`src/agent/reel_adapter.rs` — `run_request` builds `reel::AgentRequestConfig` and delegates to `reel::Agent::run()`. No tests verify grant/model/schema pass-through. The old `ClientFactory`/`ToolExecutor` injection seams were removed; `ReelAgent` always constructs a real `reel::Agent`, making the adapter untestable without live credentials. Add a `#[cfg(test)]` constructor accepting a pre-built `reel::Agent` with mock providers. **Category: Testing.**

### 7. `custom_tools: Vec::new()` allocated per agent call

`src/agent/reel_adapter.rs` — Every call to `run_request` allocates `custom_tools: Vec::new()`. `ReelAgent` never uses custom tools. Minor — could use a constant or default. **Category: Simplification.**

### 8. `RunResult` metadata discarded by `ReelAgent` adapter

`src/agent/reel_adapter.rs` — `run_request` extracts only `.output` from `reel::RunResult<T>`, discarding `usage`, `tool_calls`, and `response_hash`. The TUI metrics panel (token usage per model tier, session cost) has no data source. **Category: Feature gap.**

### 9. Output schemas missing `additionalProperties: false`

`src/agent/wire.rs` — No schema generator sets `additionalProperties: false`. LLM may produce extra fields. Some providers require this for strict structured output. **Category: Spec compliance.**

### 10. Default model names during init may not match non-Anthropic providers

`src/main.rs` — When `epic.toml` is absent, defaults use Anthropic model names. If the user's credential points to a non-Anthropic provider, init exploration fails with an opaque model error. **Category: Edge case.**

### 11. Decompose/design phases get NU grant (arbitrary shell access)

`src/agent/reel_adapter.rs` — `readonly_grant()` includes `ToolGrant::NU`, giving decompose/verify phases access to arbitrary shell commands via the NuShell tool. These phases only need file-read tools. **Category: Least privilege.**

### 12. Assess and checkpoint hardcoded to `Model::Haiku`

`src/agent/reel_adapter.rs` — `assess()` and `checkpoint()` always use `Model::Haiku`. For complex contexts or consequential decisions (checkpoint `Escalate`), Haiku may lack sufficient reasoning capacity. No override mechanism exists. **Category: Design.**

### 13. `assess_recovery` uses `Model::Opus` with no tools

`src/agent/reel_adapter.rs` — Recovery assessor gets `ToolGrant::empty()` so it cannot inspect the codebase to judge recoverability. Must rely entirely on prompt context. **Category: Design.**

### 14. Prompt injection via unsanitized `TaskContext` fields

`src/agent/prompts.rs` — All `TaskContext` fields (goal, discoveries, guidance, rationale) are interpolated into prompts without sanitization. Since goals originate from prior LLM decomposition output, a model could craft goals that manipulate subsequent calls. **Category: Security.**

### 15. Dual rationale sections in recovery prompt

`src/agent/prompts.rs` — `build_design_recovery_subtasks` appends `ctx.task.decomposition_rationale`, while `format_context` (also called) appends `ctx.parent_decomposition_rationale`. If both are populated, two rationale sections appear without clear distinction. **Category: Clarity.**

### 16. No case/whitespace normalization on wire type string fields

`src/agent/wire.rs` — All string matching (`"leaf"`, `"haiku"`, `"small"`, etc.) is exact. LLMs may return `"Leaf"`, `" leaf"`, or `"LEAF"`. Adding `.trim().to_lowercase()` before matching would improve robustness. **Category: Robustness.**

### 17. README describes lot as "via reel" but epic depends on lot directly

`README.md` — epic calls `lot::appcontainer_prerequisites_met` and `lot::grant_appcontainer_prerequisites` directly for Windows setup. The dependency is legitimate (CLI concern, not agent session concern) but the README is misleading. **Category: Documentation.**
