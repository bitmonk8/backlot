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

### 18. TUI `VaultBootstrapCompleted` handler doesn't track cost

`src/tui/mod.rs` — The `VaultBootstrapCompleted` event handler adds a worklog entry but does not add `cost_usd` to `self.total_cost_usd`. Vault record/reorganize costs are tracked (via `accumulate_usage` → `UsageUpdated`), but bootstrap cost is omitted from the TUI running cost total. **Category: Correctness.**

### 19. `std::mem::forget(tmp)` leaks TempDir in test helper

`src/knowledge.rs` — `make_dummy_vault()` calls `std::mem::forget(tmp)` to keep the TempDir alive, but this leaks directories on every test run. Should return the TempDir alongside the vault so it is dropped at test end. **Category: Testing.**

### 20. No orchestrator tests for vault integration paths

`src/orchestrator.rs` — `record_to_vault`, `reorganize_vault`, and all 4 integration points (discoveries, verification failure, checkpoint adjust, recovery) have zero test coverage. Vault is always `None` in existing tests. Testing requires either a trait abstraction for vault or a tempdir-based vault with mock providers. **Category: Testing.**

### 21. `ResearchTool::execute` untested

`src/knowledge.rs` — Three branches (empty question error, successful query, query failure) have no test coverage. The empty-question branch could be tested with the existing `make_dummy_vault` helper. **Category: Testing.**

### 22. Vault cost folding in `run_request` untested

`src/agent/reel_adapter.rs` — When `with_research` is true and vault is attached, the code drains the research sink and accumulates token counts/costs into session metadata. This field-by-field arithmetic has no test verifying correctness. **Category: Testing.**

### 23. SessionMeta field-by-field accumulation is fragile

`src/agent/reel_adapter.rs` — Vault cost folding manually adds 7 fields of `SessionMeta`. If `SessionMeta` gains a field, this code silently omits it. Should be an `AddAssign` impl or `merge` method on `SessionMeta`. **Category: Fragility.**

### 24. Vault construction duplicates registry building

`src/main.rs` — Vault construction builds `ModelRegistry` and `ProviderRegistry` a second time (identical to what `ReelAgent::new` does internally). Should share the registries or extract a common factory. **Category: Simplification.**

### 25. `SessionMeta::from_vault` placed far from type definition

`src/knowledge.rs` — `from_run_result` lives in `src/agent/mod.rs` near `SessionMeta`'s definition, but `from_vault` is in `src/knowledge.rs`. Splits the type's constructor API across two files. Should be consolidated in `agent/mod.rs`. **Category: Placement.**

### 26. `vault_content` variable name is directionally confusing

`src/orchestrator.rs` — At lines ~852 and ~1176, `vault_content` holds content destined *for* the vault, but the name reads as content *from* the vault. Consider `content_for_vault` or `findings_to_record`. **Category: Naming.**

### 27. Module `knowledge.rs` name doesn't match contents

`src/knowledge.rs` — Named `knowledge` but contains vault-integration glue: tool handler, metadata conversion, formatting. A name like `vault_bridge` would better describe the actual contents. **Category: Naming.**

### 28. `record_findings` called per-gap instead of batched

`src/knowledge.rs` — Each gap's exploration findings trigger a separate `vault.record()` call (each involves a librarian LLM call). Batching all findings into a single record call after the exploration loop would reduce vault LLM costs. **Category: Performance.**

### 29. `run_pipeline` has no test coverage (concrete dependencies)

`src/knowledge.rs` — `ResearchTool` takes concrete `Arc<vault::Vault>` and `Arc<reel::Agent>`. Neither type is behind a trait, so `run_pipeline`'s 6+ branching paths (short-circuits, fallbacks, exploration loop) cannot be unit-tested. Extracting a trait or using a callback-based design would enable testing. **Category: Testing.**

### 30. Document name collision from 40-char truncation

`src/knowledge.rs` — `record_findings` generates vault document names by taking the first 40 alphanumeric chars of the question. Different questions with identical prefixes produce the same document name, causing unrelated findings to merge via the Append fallback. **Category: Correctness.**

### 31. `ResearchScope::Project` name hides vault-inclusive behavior

`src/knowledge.rs` — `Project` scope means "vault + codebase exploration" but the name implies codebase-only. A name like `VaultAndProject` or `Full` would be clearer. **Category: Naming.**

### 32. Hand-coded JSON schemas rebuilt on every call

`src/knowledge.rs` — `gap_analysis_schema()`, `exploration_result_schema()`, and `synthesis_schema()` build `serde_json::Value` via `json!()` on every invocation. Could use `LazyLock` statics. Risk of schema/struct drift since schemas are manually maintained. **Category: Simplification.**

### 33. Wire types and schemas not in `agent/wire.rs`

`src/knowledge.rs` — The 4 internal wire types (`GapAnalysis`, `ExplorationResult`, `Finding`, `SynthesisResult`) and 3 schema generators break the project convention of placing all wire types in `src/agent/wire.rs`. **Category: Placement.**

### 34. `TempDir::new()` in knowledge tests uses system temp

`src/knowledge.rs` — `make_dummy_vault()` uses `TempDir::new()` which creates under `%TEMP%`. Per CLAUDE.md, AppContainer sandboxing requires project-local dirs. Should use `TempDir::new_in()`. **Category: Testing.**

### 35. Pre-existing: stale test names reference old NU grant

`src/agent/reel_adapter.rs` — `execute_grant_includes_write_and_nu` and `readonly_grant_includes_nu_not_write` reference the old `NU` grant name (now `TOOLS`). **Category: Cruft.**

### 36. Pre-existing: stale NU references in README/DESIGN

`README.md` — References old `NU` grant name at lines 52, 61, 64-66. `docs/DESIGN.md` — Per-Phase Tool Grants table uses `NU` at lines 113-116. **Category: Cruft.**

### 37. `file_level_review` in `ReelAgent` is verbatim copy of `verify`

`src/agent/reel_adapter.rs` — `file_level_review` is identical to `verify` except for the prompt builder call. Both construct `verification_schema()`, call `run_request` with `readonly_grant()`, and `TryFrom` the wire type. Extract a shared helper parameterized by `PromptPair`. **Category: Simplification.**

### 38. Duplicate failure-routing in `finalize_branch` for file-level review

`src/orchestrator.rs` — When file-level review fails in `finalize_branch`, the failure-handling logic (is_fix_task check, routing to `fail_task` vs `leaf_retry_loop`) duplicates the `VerificationOutcome::Fail` arm directly below it. Could fall through to the existing failure-handling code. **Category: Simplification.**

### 39. `finalize_branch` reimplements verify+review inline instead of calling `try_verify`

`src/orchestrator.rs` — `finalize_branch` runs verification and file-level review inline rather than delegating to `try_verify` (which already encapsulates both). The two code paths must be kept in sync manually. **Category: Separation.**

### 40. Missing graceful error degradation in `try_file_level_review`

`src/orchestrator.rs` — When `file_level_review()` returns `Err(e)`, the `?` operator propagates it as a fatal `OrchestratorError::Agent`, aborting the run. By contrast, `try_verify` catches agent errors and degrades to `VerifyOutcome::Failed`. A transient agent error during file-level review crashes the run. **Category: Correctness.**

### 41. `branch_skips_file_level_review` test relies on event assertion only

`src/orchestrator.rs` — The test verifies branches skip file-level review by checking for zero `FileLevelReviewCompleted` events for the root task. It cannot distinguish "correctly skipped the call" from "incorrectly called but returned Pass." A stronger test would verify the agent method is never invoked. **Category: Testing.**

### 42. Duplicate `record_to_vault` across orchestrator and task

`src/orchestrator/mod.rs` and `src/task/leaf.rs` — Both implement the same vault recording logic (try New, fallback to Append on VersionConflict, accumulate usage, emit event). The orchestrator version is used for branch operations; the task version for leaf operations. Should be consolidated. **Category: Separation.**

### 43. Duplicate `try_verify`/`try_file_level_review` across orchestrator and task

`src/orchestrator/mod.rs` and `src/task/leaf.rs` — Both implement verify + file-level-review logic. The orchestrator version serves branch paths; the task version serves leaf paths. Core agent interaction is identical. **Category: Separation.**

### 44. Duplicate `check_scope` across leaf and branch task modules

`src/task/leaf.rs` (`check_scope`) and `src/task/branch.rs` (`check_branch_scope`) — Both extract magnitude and project_root, call `git_diff_numstat`, call `evaluate_scope`. Same logic in two Task methods. Should be consolidated into a shared helper on Task. **Category: Separation.**

### 45. `__agent_error__` sentinel string for error propagation

`src/task/leaf.rs` and `src/orchestrator/mod.rs` — `Task::execute_leaf` returns `TaskOutcome::Failed { reason: "__agent_error__: ..." }` and the orchestrator parses it with `strip_prefix`. Stringly-typed error channel. A `Result<TaskOutcome, anyhow::Error>` return type would eliminate this. **Category: Design.**

### 46. (Resolved) `ChildResponse` now used; `BranchResult` removed

`src/task/branch.rs` — `ChildResponse` is now used by `handle_checkpoint` and the orchestrator's checkpoint handling. `BranchResult` was removed as dead code. **Category: Resolved.**

### 47. `emit_usage_event` sends `phase_cost_usd: 0.0`

`src/task/leaf.rs` — Task-level `emit_usage_event` always sends `phase_cost_usd: 0.0` while the orchestrator's `accumulate_usage` sends the actual value. Inconsistent usage event data. **Category: Correctness.**

### 48. DESIGN.md describes unimplemented features as current

`docs/DESIGN.md` — Simplification review (line 52), aggregate simplification (line 64), and user-level config (line 713) are described as current behavior but are listed as not implemented in STATUS.md. **Category: Documentation.**

### 49. Testing gaps from orchestrator refactor

`src/task/leaf.rs` has ~450 lines with zero unit tests. `src/task/mod.rs` mutation methods (`trailing_attempts_at_tier`, `record_attempt`, `record_discoveries`, `append_checkpoint_guidance`) lack unit tests. `src/task/scope.rs` missing tests for `lines_deleted` and `lines_modified` exceeded paths. **Category: Testing.**

### 50. `ancestor_goals` may duplicate parent goal

`src/orchestrator/context.rs` — `TreeContext::ancestor_goals` includes the immediate parent's goal, which is also available via `parent_goal`. Consumers iterating `ancestor_goals` get the parent goal twice if they also check `parent_goal`. **Category: Correctness.**

### 51. Test uses `std::env::temp_dir()` for checkpoint test

`src/orchestrator/mod.rs` — `checkpoint_saves_state` test uses `std::env::temp_dir()`. Per CLAUDE.md, AppContainer sandboxing requires project-local dirs. Should use `TempDir::new_in()` with a project-local path. **Category: Testing.**

### 52. `BranchVerifyOutcome` duplicates `VerifyOutcome`

`src/task/branch.rs` — `BranchVerifyOutcome { Passed, Failed { reason } }` is structurally identical to `task::verify::VerifyOutcome { Passed, Failed(String) }`. Could reuse `VerifyOutcome` and eliminate the redundant type. **Category: Simplification.**

### 53. Duplicated supersede_pending loop in orchestrator

`src/orchestrator/mod.rs` — The loop marking pending children as `Failed` and emitting `TaskCompleted` events appears in both `execute_branch` (checkpoint escalation) and `attempt_recovery` (child failure). Same pattern, ~20 lines each. Should extract an `apply_recovery_plan` or `supersede_pending_children` helper. **Category: Separation.**

### 54. Recovery eligibility policy split across Task and Orchestrator

`src/task/branch.rs` (`handle_checkpoint` lines 311-319) and `src/orchestrator/mod.rs` (`attempt_recovery` lines 900-912) — Both check `is_fix_task` and `recovery_budget_check` before recovery. Policy is duplicated across layers. Extract a shared `try_recovery` method on Task. **Category: Separation.**

### 55. Event emission in `assess_and_design_recovery` violates stated design principle

`src/task/branch.rs` — `assess_and_design_recovery` emits `RecoveryStarted` and `RecoveryPlanSelected` events and records to vault. The file's header comment states Task methods contain "decision logic and self-contained operations" while coordination stays in the orchestrator. Event emission is coordination. **Category: Separation.**

### 56. No direct unit tests for branch Task methods

`src/task/branch.rs` — 7 new Task methods (`verify_branch`, `fix_round_budget_check`, `design_fix`, `recovery_budget_check`, `assess_and_design_recovery`, `handle_checkpoint`, `check_branch_scope`) are tested only indirectly through orchestrator integration tests. Direct unit tests (especially for `fix_round_budget_check` boundary cases and `handle_checkpoint` three-way branching) would catch regressions independently. **Category: Testing.**

### 57. `handle_checkpoint` chains classification + adjust + full escalation pipeline

`src/task/branch.rs` — `handle_checkpoint` classifies discoveries, handles adjust (vault + events), and on escalate runs the full recovery pipeline (budget check, assess, design). The escalation arm (~30 lines) could be extracted into `escalate_to_recovery` for independent testing and reuse. **Category: Separation.**
