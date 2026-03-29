# Test Suite Audit Report

## Executive Summary

| Metric | Value |
|--------|-------|
| Total tests | 265 |
| Total test code | ~8,300 lines |
| Total production code | ~6,850 lines |
| Test-to-production ratio | 1.21:1 |
| Suite execution time | 0.47s (all tests < 2ms) |
| Tests recommended for removal | 13 |
| Tests recommended for merging | ~30 (into ~10 parameterized tests) |
| Tests recommended for refactoring | Orchestrator mock setup (systemic) |
| Coverage gaps identified | 18 |

Execution time is negligible across the board. The meaningful cost axis is **maintenance burden** — dominated by the 72 orchestrator integration tests (64% of test code) that use verbose `MockAgentService` queue setup.

## Methodology

Dual-axis analysis using 40 parallel agents (20 cost, 20 value), each analyzing ~13 tests.

**Cost metrics**: line count, setup complexity (1-5), mock complexity (0-5), maintenance burden (LOW/MED/HIGH), duplication notes.

**Value metrics**: category (FUNDAMENTAL/EDGE_CASE/VALIDATION/INTEGRATION/REGRESSION), coverage uniqueness (1-5), failure signal quality (1-5), protection level (LOW/MED/HIGH/CRITICAL).

**Composite score**: `value_score - cost_score` where `value = protection_numeric + uniqueness`, `cost = maintenance_numeric + mock_complexity`.

**Action thresholds**: REMOVE = LOW protection + uniqueness <= 2. MERGE = duplication noted + uniqueness < 3. REFACTOR = HIGH+ protection but HIGH maintenance.

---

## Module-by-Module Analysis

### agent::prompts (21 tests, ~369 lines)

All tests are zero-mock string-contains checks against prompt builder output. Setup is uniformly a shared `test_context()` helper.

**Pattern**: Call `build_*()`, assert substrings in `query` and `system_prompt`. Maintenance burden is MEDIUM because prompt template wording changes break substring assertions.

**Duplication cluster**: The `*_contains_context` family (`assess`, `execute`, `decompose`, `design_fix_subtasks`, `file_level_review`, `verify`) follows an identical pattern. Each is 5-12 lines. Could be collapsed into one parameterized test.

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

### agent::wire (23 tests, ~317 lines)

Zero-mock wire-format round-trip tests. Construct a `*Wire` struct, call `TryFrom`, assert fields.

**Duplication clusters**:
1. Assessment trio (`roundtrip`, `with_magnitude`, `partial_magnitude`) — could be parameterized
2. Empty-subtask rejection pair (`recovery_plan_wire_empty_subtasks_rejected` vs `full_approach_empty_subtasks_rejected`) — differ only in `approach` field, same guard fires
3. Four `*_schema_has_required_fields` tests — identical structure, different schema function

**Removal candidate**: `init_uses_sonnet_model_key` — exact duplicate of `model_key_mapping` in reel_adapter.

**Gaps**:
- `verification_wire_fail` variant missing (only `pass` tested)
- No invalid-outcome-string rejection tests for `TaskOutcomeWire` or `VerificationWire`
- `CheckpointWire` "escalate" variant never tested
- Invalid model name in `parse_model_name` untested

### cli (3 tests, ~23 lines)

Trivial `try_parse_from` checks. LOW cost, MEDIUM value. No action needed.

### config::project (32 tests, ~343 lines)

Formulaic validation tests. All zero-mock, 1-12 lines, LOW maintenance.

**Duplication cluster**: Six `validate_*_zero` tests follow identical 5-line pattern (mutate default, validate, assert error). Could be one parameterized test.

**Removal candidates**:
- `default_config_partial_eq` — tests compiler-derived PartialEq (1 line, zero value)
- `default_max_total_tasks` — covered by `default_config_round_trips` and `parse_minimal_config`
- `vault_disabled_by_default_in_empty_config` — subset of `vault_config_defaults`

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

### knowledge (24 tests, ~357 lines)

Deserialization/formatting tests. Mostly 3-15 lines, zero mocks.

**Removal candidates**:
- `scope_from_str_project` — same code path as `scope_from_str_defaults_to_project`
- `format_query_result_full_coverage` and `format_query_result_no_extracts` — test a test-only helper function, not production code
- `synthesis_result_deserialize_no_refs` — near-identical to `synthesis_result_deserialize`

**Merge candidates**: Three `*_schema_is_valid_json` tests — identical pattern.

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

### orchestrator::tests (72 tests, ~5,351 lines)

**This is the primary audit target — 64% of all test code.**

See [Orchestrator Tests Deep-Dive](#orchestrator-tests-deep-dive) below.

### sandbox (8 tests, ~130 lines)

Trivial 1-3 line checks against `model_indicates_vm`. All LOW cost.

**Merge candidate**: 5 vendor-specific `model_detects_*` tests could be parameterized, but the cost savings are negligible.

**Gap**: No test for case-sensitivity edge cases or partial substring matches (e.g., "NotVirtuallyAnything").

### state (19 tests, ~313 lines)

Two clusters: DFS ordering (11 tests) and state loading (8 tests). All zero-mock.

**Highest-value tests**:
- `load_repairs_next_id_below_max_task_id` (CRITICAL) — sole test for the next_id repair guard preventing duplicate TaskIds
- `persistence_round_trip` (CRITICAL) — sole integration test for full save/load cycle
- `dfs_order_self_cycle`, `dfs_order_mutual_cycle` (HIGH) — guard against infinite loops

**Merge candidates**: Four `load_*_errors` tests follow identical tempdir-write-assert-err pattern.

**Removal candidate**: `task_count_tracks_insertions` — tests `HashMap::len()` wrapper.

**Gap**: No test for `next_id` `checked_add` overflow path.

### task (28 tests across mod/branch/leaf/scope)

Mostly trivial unit tests (3-17 lines). Two async scope tests.

**Removal candidates** (derived-trait smoke tests):
- `task_path_equality` — tests derived PartialEq on 2-variant enum
- `magnitude_estimate_equality` — tests derived PartialEq
- `leaf_result_equality` — tests derived PartialEq

**Merge candidates**:
- 4 `fix_budget_*` tests → parameterized
- 4 `verification_model_*` tests → parameterized

**Highest-value tests**:
- `task_phase_valid_transitions` (CRITICAL) — exhaustive state machine specification
- `evaluate_scope_exceeded` (CRITICAL) — core scope circuit breaker
- `model_escalate_chain` (HIGH) — Haiku→Sonnet→Opus→None
- `model_ordering_haiku_lt_sonnet_lt_opus` (HIGH) — Ord correctness
- `task_new_defaults` (HIGH) — catches missing defaults on new fields
- `verification_model_leaf_opus_capped_to_sonnet` (HIGH) — cost capping
- `verification_model_branch_always_sonnet` (HIGH) — cost capping

**Gap**: `evaluate_scope` exceeded for `lines_modified` or `lines_deleted` (only `lines_added` has exceeded-path test).

### tui (9 tests, ~117 lines)

Simple event-handler tests with shared `app()` helper. All LOW cost.

**Highest-value tests**:
- `task_phase_valid_transitions` (from task module, referenced by TUI)
- `task_completion_clears_current` — prevents stale active-task display
- `task_registration_sets_root` — anchor for tree rendering

**Gap**: `UsageUpdated` event handler is untested. `KeyCode::Down` scroll handler tested by formula duplication, not actual keypress.

---

## Recommendations

### Tests to Remove

| Test | Reason | Risk |
|------|--------|------|
| `init_uses_sonnet_model_key` | Exact duplicate of `model_key_mapping` in reel_adapter | None |
| `default_config_partial_eq` | Tests compiler-derived PartialEq on `EpicConfig::default()` | None |
| `scope_from_str_project` | Same code path as `scope_from_str_defaults_to_project` | None |
| `format_query_result_full_coverage` | Tests a test-only helper, not production code | None |
| `format_query_result_no_extracts` | Tests a test-only helper, not production code | None |
| `task_path_equality` | Tests derived PartialEq on 2-variant enum | None |
| `magnitude_estimate_equality` | Tests derived PartialEq on simple enum | None |
| `leaf_result_equality` | Tests derived PartialEq | None |
| `task_count_tracks_insertions` | Tests `HashMap::len()` wrapper; covered transitively | None |
| `synthesis_result_deserialize_no_refs` | Near-identical to `synthesis_result_deserialize` | Negligible |
| `default_max_total_tasks` | Covered by `default_config_round_trips` + `parse_minimal_config` | Negligible |
| `schema_without_tools_omits_tools_key` | Asserts negative property of a static JSON literal | None |
| `branch_fix_mixed_errors_then_success` | Superset behavior covered by individual error tests | Low |

**Total lines saved**: ~90

### Tests to Merge (into parameterized tests)

| Tests to Combine | Count | Into | Rationale |
|-----------------|-------|------|-----------|
| `validate_*_zero` (branch_fix_rounds, max_depth, max_recovery_rounds, retry_budget, root_fix_rounds, max_total_tasks) | 6 | 1 parameterized | Identical 5-line pattern, differ only in field name |
| `*_schema_has_required_fields` (assessment, decomposition, verification, task_outcome) | 4 | 1 parameterized | Identical structure, different schema fn |
| `*_contains_context` prompt tests (assess, execute, decompose, design_fix, file_level_review, verify) | 6 | 1 parameterized | Same pattern: build prompt, assert goal in query |
| `fix_budget_*` (within_budget_sonnet, opus_round, exhausted_nonroot, exhausted_root) | 4 | 1 table-driven | Same setup, differ in rounds/root flag/expected result |
| `verification_model_*` (leaf_haiku, leaf_sonnet, leaf_opus_capped, branch_always_sonnet) | 4 | 1 table-driven | Same setup, differ in path/model/expected |
| `load_*_errors` (empty_file, invalid_json, wrong_field_types, wrong_schema) | 4 | 1 parameterized | Same tempdir-write-assert-err pattern |
| `*_schema_is_valid_json` (gap_analysis, exploration_result, synthesis) | 3 | 1 parameterized | Identical validation pattern |
| `recovery_plan_wire_empty_subtasks_rejected` + `full_approach_empty_subtasks_rejected` | 2 | 1 | Same guard, different approach string |
| `depth_cap_forces_leaf` + `custom_max_depth_forces_leaf` | 2 | 1 | Same behavior, keep the explicit-config version |
| `recovery_full_redecomposition_skips_pending` + `preserves_completed_siblings` | 2 | 1 | 3-child variant subsumes 2-child |

**Total tests eliminated by merging**: ~30 reduced to ~10 (net -20 tests)
**Total lines saved**: ~200-250

### Tests to Refactor (Reduce Cost)

| Area | Current Cost | Suggestion |
|------|-------------|------------|
| **MockAgentService queue setup** (all 72 orchestrator tests) | 4-line ceremony per mock response (`lock().unwrap().push_back()`) | Extract a builder: `MockBuilder::new().assess(leaf).execute(success).verify(pass).build()`. Would cut 30-40% of orchestrator test line count (~1,500-2,000 lines). |
| **Resume test state construction** (6 tests) | Manual `Task` field-by-field construction, 15-25 lines each | Extract `make_mid_execution_state(phase, model, attempts)` helper |
| `checkpoint_escalate_clears_prior_guidance` | 153 lines, highest in suite | Multi-phase test; could split into setup helper + focused assertion |
| **Scope test `Magnitude` literals** | Same `Magnitude { ... }` literal repeated 5 times | Extract to a `const TEST_MAGNITUDE` |

### Coverage Gaps

| Module | Missing Coverage | Suggested Priority |
|--------|-----------------|----------|
| agent::wire | `verification_wire_fail` variant, `CheckpointWire::Escalate` variant | High — boundary between LLM output and domain types |
| agent::wire | Invalid outcome strings for `TaskOutcomeWire`, `VerificationWire` | High — malformed LLM output rejection |
| agent::wire | Invalid model name in `parse_model_name` | Medium |
| agent::prompts | `build_explore_for_init` prompt content | Low — hardcoded strings |
| agent::reel_adapter | Grant wiring per `AgentService` method (correct grant chosen per phase) | Medium — tested at function level but not at wiring level |
| task::scope | `evaluate_scope` exceeded for `lines_modified` / `lines_deleted` | Medium — only `lines_added` exceeded path tested |
| task::scope | Integration test where git succeeds and `Exceeded` propagates through `check_branch_scope` | Medium |
| tui | `UsageUpdated` event handler | Low |
| state | `next_id` `checked_add` overflow | Low — u64 overflow is improbable |
| config | `EpicConfig::load` with permission denied | Low |
| orchestrator | Resume from crash mid-recovery-subtask execution | Medium |
| orchestrator | `branch_fix_rounds=0` / `root_fix_rounds=0` clamping | Low — parallel to existing `retry_budget` test |
| init | `edit_step` returning None on empty command | Low |
| init | EOF mid-interaction (`read_line_checked` bail) | Low |
| knowledge | `MAX_GAPS` (5) cap enforcement | Low |
| sandbox | Partial substring false positive (e.g., "NotVirtuallyAnything") | Low |
| tui | Duplicate child registration guard | Low |
| config | `ModelConfig::name_for` method | Low |

---

## Orchestrator Tests Deep-Dive

72 tests, 5,351 lines — 64% of all test code. All use `MockAgentService` with elaborate mock choreography.

### Mock Setup is the Dominant Cost

Every test manually pushes responses into `Mutex<VecDeque<T>>` queues:
```rust
mock.assessments.lock().unwrap().push_back(leaf_assessment(Model::Haiku));
mock.leaf_results.lock().unwrap().push_back(Ok(leaf_success()));
mock.verifications.lock().unwrap().push_back(Ok(pass_verification()));
mock.file_level_reviews.lock().unwrap().push_back(Ok(pass_file_level_review()));
```

This 4-line-per-response ceremony accounts for an estimated 2,500-3,000 lines across the 72 tests.

### Builder Pattern Recommendation

```rust
// Before: ~20 lines
let mock = MockAgentService::new();
mock.assessments.lock().unwrap().push_back(branch_assessment(Model::Haiku));
mock.decompositions.lock().unwrap().push_back(Ok(one_subtask_decomposition()));
mock.assessments.lock().unwrap().push_back(leaf_assessment(Model::Haiku));
mock.leaf_results.lock().unwrap().push_back(Ok(leaf_success()));
mock.verifications.lock().unwrap().push_back(Ok(pass_verification()));
mock.file_level_reviews.lock().unwrap().push_back(Ok(pass_file_level_review()));
mock.verifications.lock().unwrap().push_back(Ok(pass_verification()));

// After: ~8 lines
let mock = MockBuilder::new()
    .branch(Model::Haiku)
    .decompose_one()
    .leaf_happy_path(Model::Haiku)
    .branch_verify_pass()
    .build();
```

### Parameterization Candidates

| Test Family | Tests | Pattern |
|-------------|-------|---------|
| No-recursive-fix guards | `branch_fix_subtask_no_recursive_fix_loop`, `branch_fix_subtasks_no_recursive_fix`, `leaf_fix_subtask_no_recursive_fix_loop`, `fix_task_file_review_fail_no_fix_loop` | Same invariant (fix tasks skip fix loop), different structural paths |
| Checkpoint escalation outcomes | `escalate_triggers_recovery`, `escalate_unrecoverable_fails`, `escalate_on_fix_task_fails`, `escalate_recovery_rounds_exhausted` | Same escalation setup, different terminal decisions |
| Resume entry points | `resume_skips_completed_child`, `resume_skips_decomposition_when_subtasks_exist`, `resume_mid_execution_branch_not_reassessed`, `resume_verifying_skips_execution` | Same manual state construction, different phase/assertion |
| Custom limits | `custom_branch_fix_rounds`, `custom_root_fix_rounds`, `custom_retry_budget`, `custom_max_recovery_rounds`, `custom_max_depth` | Same `with_limits` pattern, different config field |
| Execution vs fix escalation | `leaf_retry_and_escalation` / `leaf_fix_escalates_model` | Same Haiku→Sonnet pattern, execution vs fix path |

### Tests with Unique High Value (do not touch)

| Test | Why |
|------|-----|
| `checkpoint_saves_state` | Only disk-persistence test |
| `single_leaf` | Canonical happy-path |
| `terminal_failure` | Full 9-attempt exhaustion |
| `checkpoint_escalate_triggers_recovery` | Full escalation→recovery pipeline |
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
| `branch_fix_mixed_errors_then_success` | Individual error tests | REMOVE — covered by `design_error_retries` + `verify_error_retries` |
| `recovery_full_redecomposition_skips_pending` | `recovery_full_redecomp_preserves_completed_siblings` | MERGE — 3-child variant subsumes 2-child |
| `file_level_review_pass_completes` | Many tests queue passing file reviews | KEEP — sole test asserting `FileLevelReviewCompleted` event |
| `checkpoint_guidance_persisted` | `checkpoint_multiple_adjusts_accumulates_guidance` | KEEP — persistence (serde) vs accumulation are distinct concerns |

---

## Summary of Recommended Changes

| Action | Count | Lines Saved |
|--------|-------|-------------|
| Remove tests | 13 | ~90 |
| Merge into parameterized | ~30 → ~10 | ~200-250 |
| MockBuilder pattern (orchestrator) | Systemic | ~1,500-2,000 |
| **Total** | | **~1,800-2,300** |

This would reduce test code from ~8,300 to ~6,000-6,500 lines (keeping the test-to-production ratio near 1:1) while maintaining the same effective coverage. The 18 coverage gaps should be addressed with ~18 new focused tests (~200-300 lines), bringing the net reduction to ~1,500-2,000 lines.
