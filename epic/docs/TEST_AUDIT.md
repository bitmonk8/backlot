# Test Suite Audit Report

## Executive Summary

| Metric | Value |
|--------|-------|
| Total tests | 235 |
| Total test code | ~8,530 lines |
| Total production code | ~6,375 lines |
| Test-to-production ratio | 1.34:1 |
| Suite execution time | 0.44s (all tests < 2ms) |

Execution time is negligible across the board. The meaningful cost axis is **maintenance burden** — dominated by the 71 orchestrator integration tests (62% of test code) that use verbose `MockAgentService` queue setup.

## Methodology

Dual-axis analysis using 40 parallel agents (20 cost, 20 value), each analyzing ~13 tests.

**Cost metrics**: line count, setup complexity (1-5), mock complexity (0-5), maintenance burden (LOW/MED/HIGH), duplication notes.

**Value metrics**: category (FUNDAMENTAL/EDGE_CASE/VALIDATION/INTEGRATION/REGRESSION), coverage uniqueness (1-5), failure signal quality (1-5), protection level (LOW/MED/HIGH/CRITICAL).

**Composite score**: `value_score - cost_score` where `value = protection_numeric + uniqueness`, `cost = maintenance_numeric + mock_complexity`.

---

## Module-by-Module Analysis

### agent::prompts (17 tests, ~351 lines)

All tests are zero-mock string-contains checks against prompt builder output. Setup is uniformly a shared `test_context()` helper.

**Pattern**: Call `build_*()`, assert substrings in `query` and `system_prompt`. Maintenance burden is MEDIUM because prompt template wording changes break substring assertions.

**Parameterized test**: `prompt_builders_contain_context` covers 5 builders (assess, execute, decompose, verify, file_level_review) in a single table-driven test, checking both query goal and system_prompt role keyword.

**Highest-value tests**:
- `checkpoint_prompt_with_populated_children` — sole test for all 4 ChildStatus variants
- `context_format_includes_checkpoint_guidance` — sole test for checkpoint guidance rendering
- `context_format_with_no_siblings` — sole test for root/empty edge case
- `scope_limiting_instructions_in_prompts` — sole test for scope-limiting language in 3 builders

**Gap**: `build_explore_for_init` has zero tests. Sibling/child discovery rendering has no explicit assertions.

### agent::reel_adapter (5 tests, ~65 lines)

Trivial 1-3 line value checks. Zero mocks.

**Highest-value tests**:
- `build_model_registry_produces_correct_entries` (CRITICAL) — only test for model registry wiring
- `execute_grant_includes_write_and_nu` (CRITICAL) — only test that execute phases get WRITE
- `readonly_grant_includes_nu_not_write` (CRITICAL) — only test that read-only phases lack WRITE

**Overlap**: `model_key_mapping` and `default_max_tokens_per_tier` are transitively covered by `build_model_registry_produces_correct_entries`.

**Gap**: No test verifies the correct grant is chosen per `AgentService` method (grant functions are tested, but wiring is not).

### agent::wire (22 tests, ~351 lines)

Zero-mock wire-format round-trip tests. Construct a `*Wire` struct, call `TryFrom`, assert fields.

**Parameterized tests**:
- `schemas_have_required_fields` — table-driven check of required fields across 4 schema functions
- `recovery_plan_wire_empty_subtasks_rejected` — loop over `["incremental", "full"]` approaches

**Highest-value tests**:
- `verification_wire_fail` + `verification_wire_fail_no_reason_defaults` — boundary between LLM output and domain types for fail path
- `assessment_wire_invalid_model_rejected` — rejects unknown model names
- `task_outcome_wire_invalid_outcome_rejected` / `verification_wire_invalid_outcome_rejected` — malformed LLM output rejection

**Gaps**:
- `DetectedStepWire` conversion: default timeout (300) when `timeout` is `None`
- `SubtaskWire` with invalid magnitude (e.g., `"huge"`)

### cli (3 tests, ~23 lines)

Trivial `try_parse_from` checks. LOW cost, MEDIUM value. No action needed.

### config::project (25 tests, ~284 lines)

Formulaic validation tests. All zero-mock, 1-12 lines, LOW maintenance.

**Parameterized test**: `validate_zero_fields_rejected` covers 6 fields (max_depth, max_recovery_rounds, retry_budget, branch_fix_rounds, root_fix_rounds, max_total_tasks) in one table-driven test.

**Highest-value tests**:
- `load_nonexistent_returns_default` (CRITICAL) — first-run without config
- `load_valid_file` (CRITICAL) — full happy-path load
- `validate_default_config_passes` — broken-out-of-box guard
- `vault_disabled_skips_model_validation` — conditional validation bypass

**Gaps**: No test for `load` with permission errors. No upper-bound tests for `retry_budget`, `root_fix_rounds`, `branch_fix_rounds`.

### init (17 tests, ~235 lines)

Lightweight `mock_lines` helper (canned stdin). Each test documents a distinct interactive path.

All tests are well-justified. `present_and_confirm_add_custom_step` is the most complex (18 lines) and uniquely tests the "add another step" loop.

**Gap**: No test for `edit_step` returning `None` on empty command. No test for EOF mid-interaction.

### knowledge (18 tests, ~291 lines)

Deserialization/formatting tests. Mostly 3-15 lines, zero mocks.

**Parameterized test**: `schemas_are_valid_json_objects` — table-driven check of 3 schema functions (type + expected properties).

**Highest-value tests**:
- `from_vault_metadata_maps_fields` — sole test for 7-field vault-to-epic conversion
- `research_tool_definition_schema` — sole test for tool API contract
- `synthesis_result_deserialize_default_refs` — sole test for `#[serde(default)]` on optional field

**Gap**: `MAX_GAPS` cap (5) is untested.

### orchestrator::context (3 tests, ~202 lines)

Mock-free state-construction tests.

All three are HIGH value:
- `child_status_mapping_all_phases` — exhaustive phase-to-status mapping (sole test)
- `populates_parent_fields_and_children` — sole test for parent field propagation
- `skips_dangling_subtask_id` — sole test for corrupted-state resilience

No action needed.

### orchestrator::tests (71 tests, ~5,284 lines)

**This is the primary maintenance concern — 62% of all test code.**

See [Orchestrator Tests Deep-Dive](#orchestrator-tests-deep-dive) below.

### sandbox (8 tests, ~130 lines)

Trivial 1-3 line checks against `model_indicates_vm`. All LOW cost.

**Gap**: No test for case-sensitivity edge cases or partial substring matches (e.g., "NotVirtuallyAnything").

### state (16 tests, ~285 lines)

Two clusters: DFS ordering (11 tests) and state loading (5 tests). All zero-mock.

**Parameterized test**: `load_invalid_content_errors` — table-driven check of 4 invalid file contents (empty, bad JSON, wrong schema, wrong field types).

**Highest-value tests**:
- `load_repairs_next_id_below_max_task_id` (CRITICAL) — sole test for the next_id repair guard preventing duplicate TaskIds
- `persistence_round_trip` (CRITICAL) — sole integration test for full save/load cycle
- `dfs_order_self_cycle`, `dfs_order_mutual_cycle` (HIGH) — guard against infinite loops

**Gap**: No test for `next_id` `checked_add` overflow path.

### task (21 tests across mod/branch/leaf/scope)

Mostly trivial unit tests (3-17 lines). Two async scope tests.

**Parameterized tests**:
- `fix_budget_check_cases` — table-driven check of 4 budget scenarios (Sonnet, Opus round, exhausted non-root, exhausted root)
- `verification_model_cases` — table-driven check of 4 model selection scenarios (leaf Haiku/Sonnet/Opus-capped, branch always Sonnet)

**Highest-value tests**:
- `task_phase_valid_transitions` (CRITICAL) — exhaustive state machine specification
- `evaluate_scope_exceeded` (CRITICAL) — core scope circuit breaker (lines_added)
- `evaluate_scope_lines_modified_exceeded` / `evaluate_scope_lines_deleted_exceeded` — remaining scope dimensions
- `model_escalate_chain` (HIGH) — Haiku->Sonnet->Opus->None
- `model_ordering_haiku_lt_sonnet_lt_opus` (HIGH) — Ord correctness
- `task_new_defaults` (HIGH) — catches missing defaults on new fields
- `verification_model_cases` (HIGH) — cost capping and branch override

### tui (9 tests, ~117 lines)

Simple event-handler tests with shared `app()` helper. All LOW cost.

**Highest-value tests**:
- `task_completion_clears_current` — prevents stale active-task display
- `task_registration_sets_root` — anchor for tree rendering

**Gap**: `UsageUpdated` event handler is untested. `KeyCode::Down` scroll handler tested by formula duplication, not actual keypress.

---

## Remaining Recommendations

### Tests to Merge (orchestrator — deferred to MockBuilder session)

| Tests to Combine | Count | Rationale |
|-----------------|-------|-----------|
| `depth_cap_forces_leaf` + `custom_max_depth_forces_leaf` | 2 | Same behavior, keep the explicit-config version |
| `branch_fix_subtasks_no_recursive_fix` + `branch_fix_subtask_no_recursive_fix_loop` | 2 | Same invariant, slight structural variation |
| `recovery_full_redecomposition_skips_pending` + `recovery_full_redecomp_preserves_completed_siblings` | 2 | 3-child variant subsumes 2-child |

### MockBuilder Pattern (orchestrator — systemic)

```rust
// Current: ~20 lines per test setup
let mock = MockAgentService::new();
mock.assessments.lock().unwrap().push_back(branch_assessment(Model::Haiku));
mock.decompositions.lock().unwrap().push_back(Ok(one_subtask_decomposition()));
mock.assessments.lock().unwrap().push_back(leaf_assessment(Model::Haiku));
mock.leaf_results.lock().unwrap().push_back(Ok(leaf_success()));
mock.verifications.lock().unwrap().push_back(Ok(pass_verification()));
mock.file_level_reviews.lock().unwrap().push_back(Ok(pass_file_level_review()));
mock.verifications.lock().unwrap().push_back(Ok(pass_verification()));

// Proposed: ~8 lines
let mock = MockBuilder::new()
    .branch(Model::Haiku)
    .decompose_one()
    .leaf_happy_path(Model::Haiku)
    .branch_verify_pass()
    .build();
```

The 4-line-per-response `lock().unwrap().push_back()` ceremony accounts for ~2,500-3,000 lines across the 71 orchestrator tests. A builder would cut 30-40% of orchestrator test code (~1,500-2,000 lines).

### Remaining Coverage Gaps

| Module | Missing Coverage | Priority |
|--------|-----------------|----------|
| agent::wire | `DetectedStepWire` default timeout, `SubtaskWire` invalid magnitude | Low |
| agent::prompts | `build_explore_for_init` prompt content | Low |
| agent::reel_adapter | Grant wiring per `AgentService` method | Medium |
| tui | `UsageUpdated` event handler | Low |
| state | `next_id` `checked_add` overflow | Low |
| config | `EpicConfig::load` with permission denied | Low |
| config | Upper-bound tests for `retry_budget`, `root_fix_rounds`, `branch_fix_rounds` | Low |
| init | `edit_step` returning None on empty command | Low |
| init | EOF mid-interaction (`read_line_checked` bail) | Low |
| knowledge | `MAX_GAPS` (5) cap enforcement | Low |
| sandbox | Partial substring false positive | Low |
| tui | Duplicate child registration guard | Low |
| config | `ModelConfig::name_for` method | Low |
| orchestrator | Resume from crash mid-recovery-subtask execution | Medium |
| orchestrator | `branch_fix_rounds=0` / `root_fix_rounds=0` clamping | Low |

---

## Orchestrator Tests Deep-Dive

71 tests, 5,284 lines — 62% of all test code. All use `MockAgentService` with elaborate mock choreography.

### Mock Setup is the Dominant Cost

Every test manually pushes responses into `Mutex<VecDeque<T>>` queues:
```rust
mock.assessments.lock().unwrap().push_back(leaf_assessment(Model::Haiku));
mock.leaf_results.lock().unwrap().push_back(Ok(leaf_success()));
mock.verifications.lock().unwrap().push_back(Ok(pass_verification()));
mock.file_level_reviews.lock().unwrap().push_back(Ok(pass_file_level_review()));
```

This 4-line-per-response ceremony accounts for an estimated 2,500-3,000 lines across the 71 tests.

### Parameterization Candidates

| Test Family | Tests | Pattern |
|-------------|-------|---------|
| No-recursive-fix guards | `branch_fix_subtask_no_recursive_fix_loop`, `branch_fix_subtasks_no_recursive_fix`, `leaf_fix_subtask_no_recursive_fix_loop`, `fix_task_file_review_fail_no_fix_loop` | Same invariant (fix tasks skip fix loop), different structural paths |
| Checkpoint escalation outcomes | `escalate_triggers_recovery`, `escalate_unrecoverable_fails`, `escalate_on_fix_task_fails`, `escalate_recovery_rounds_exhausted` | Same escalation setup, different terminal decisions |
| Resume entry points | `resume_skips_completed_child`, `resume_skips_decomposition_when_subtasks_exist`, `resume_mid_execution_branch_not_reassessed`, `resume_verifying_skips_execution` | Same manual state construction, different phase/assertion |
| Custom limits | `custom_branch_fix_rounds`, `custom_root_fix_rounds`, `custom_retry_budget`, `custom_max_recovery_rounds`, `custom_max_depth` | Same `with_limits` pattern, different config field |
| Execution vs fix escalation | `leaf_retry_and_escalation` / `leaf_fix_escalates_model` | Same Haiku->Sonnet pattern, execution vs fix path |

### Tests with Unique High Value (do not touch)

| Test | Why |
|------|-----|
| `checkpoint_saves_state` | Only disk-persistence test |
| `single_leaf` | Canonical happy-path |
| `terminal_failure` | Full 9-attempt exhaustion |
| `checkpoint_escalate_triggers_recovery` | Full escalation->recovery pipeline |
| `checkpoint_escalate_on_fix_task_fails` | Sole test for is_fix_task guard in escalation |
| `initial_verify_error_is_fatal` | Sole test for Err propagation from verify |
| `leaf_fix_subtask_no_recursive_fix_loop` | Prevents infinite recursive fix loops |
| `leaf_fix_resume_escalates_immediately_when_tier_exhausted` | Crash-resume edge case |
| `resume_mid_execution_branch_not_reassessed` | Multi-level resume correctness |
| `resume_verifying_skips_execution` | Phase-skip correctness |
| `zero_retry_budget_clamped_to_one` | Prevents zero-iteration loops |
| `recovery_depth_inherited_not_fresh` | Prevents exponential recovery cost |
| `recovery_full_redecomp_preserves_completed_siblings` | Protects finished work during re-decomposition |
| `branch_fails_when_all_children_failed` | Prevents vacuous success |

### Orchestrator Overlap Summary

| Test | Overlaps With | Verdict |
|------|--------------|---------|
| `two_children` | `single_leaf` (scaled) | KEEP — multi-child iteration is distinct |
| `depth_cap_forces_leaf` | `custom_max_depth_forces_leaf` | MERGE — keep the explicit-config version |
| `custom_retry_budget_escalates_early` | `zero_retry_budget_clamped_to_one` | KEEP — different config values |
| `branch_fix_subtasks_no_recursive_fix` | `branch_fix_subtask_no_recursive_fix_loop` | MERGE — same invariant, slight structural variation |
| `recovery_full_redecomposition_skips_pending` | `recovery_full_redecomp_preserves_completed_siblings` | MERGE — 3-child variant subsumes 2-child |
| `file_level_review_pass_completes` | Many tests queue passing file reviews | KEEP — sole test asserting `FileLevelReviewCompleted` event |
| `checkpoint_guidance_persisted` | `checkpoint_multiple_adjusts_accumulates_guidance` | KEEP — persistence (serde) vs accumulation are distinct concerns |

---

## Estimated Impact of Remaining Recommendations

| Action | Count | Lines Saved |
|--------|-------|-------------|
| Merge orchestrator pairs | ~6 -> ~3 | ~100-150 |
| MockBuilder pattern (orchestrator) | Systemic | ~1,500-2,000 |
| **Total** | | **~1,600-2,150** |

This would reduce test code from ~8,530 to ~6,400-6,900 lines (keeping the test-to-production ratio near 1:1) while maintaining the same effective coverage. The remaining coverage gaps should be addressed with ~15 new focused tests (~150-200 lines).
