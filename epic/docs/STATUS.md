# Project Status

## Current Phase

**Audit remediation in progress** — All v1 features implemented (135 tests passing). Full codebase audit executed (95 review cells, 541 findings). Config wiring, model selection, and task/recovery caps remediated; continuing hardening.

## Milestones

- [x] Design documents and open questions resolved (23/23)
- [x] Scaffold Rust project — module structure, dependencies, Clippy lints configured
- [x] Core orchestrator loop — task types, events, state, AgentService trait, DFS loop with retry/escalation, 6 tests passing
- [x] Agent runtime selected — ZeroClaw evaluated, audited, forked, then replaced by [Flick](https://github.com/bitmonk8/flick). Originally as external executable, now as library crate dependency.
- [x] Agent call wiring — `FlickAgent` implements `AgentService` via Flick library API. Config generation (JSON in-memory), structured output schemas, wire format types with `TryFrom` conversions, prompt assembly, tool loop with resume. 38 tests at milestone.
- [x] Tool execution — All 6 tools implemented: `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `bash`. Path sandboxing, size limits, timeout handling. 15 new tests (53 total).
- [x] State persistence integration — `EpicState` saves/loads via `.epic/state.json`. Orchestrator checkpoints after assessment, decomposition, child completion, and verification. `main.rs` resumes from persisted state or creates fresh. Atomic writes (write-rename), resume skips completed/failed/mid-execution tasks correctly, reuses existing decomposition, goal mismatch detection, corrupt state error handling. 5 new tests (58 total).
- [x] TUI event consumer — `TuiApp` consumes orchestrator events via ratatui + crossterm. Task tree panel (DFS with status indicators ✓/✗/▸/·, current-task cursor), worklog panel (timestamped event stream with follow-tail), metrics panel (toggle), header bar (goal, progress, elapsed). Keyboard controls: q/Ctrl-C quit, t toggle tail, m toggle metrics, ↑↓ scroll. Orchestrator runs in background tokio task, TUI in sync foreground loop. `EPIC_NO_TUI=1` for headless mode. `TaskRegistered` event added for TUI to build task tree from events alone.
- [x] Discoveries propagation — `AgentService::execute_leaf` returns `LeafResult` (outcome + discoveries). Orchestrator stores discoveries on tasks, emits `DiscoveriesRecorded` event, triggers checkpoint flow. Sibling context includes discoveries in prompt. Failed sibling reason extracted from attempts. 2 new tests (60 total).
- [x] CLI (clap) — Replaced ad-hoc env-var/arg parsing with proper `clap` derive CLI. Subcommands: `epic run <goal>`, `epic resume`, `epic status`. Global options: `--credential`, `--no-tui` (with env-var fallbacks). `status` subcommand prints goal, root phase, and task counts from persisted state. 60 tests at milestone.
- [x] `epic init` — Agent-driven interactive configuration scaffolding. Flick agent (Sonnet, read-only tools) scans project for build/test/lint markers. Interactive CLI confirms/edits/skips each step, prompts for model preferences and depth/budget limits. Writes `epic.toml` with atomic write. Declined steps included as TOML comments. `EpicConfig` struct with `VerificationStep`, `ModelConfig`, `LimitsConfig`, `AgentConfig`, `ProjectConfig`. 3 new tests (63 total).
- [x] Flick library migration — Replaced subprocess invocation with direct library calls. `flick` added as git dependency, `serde_yaml` removed. `FlickAgent` uses `FlickClient` API (no process spawning, no config/tool-result file I/O). `--flick-path` CLI option and `AgentConfig.flick_path` removed. Config built as JSON in-memory, parsed via `Config::from_str()`. Review pass added 3 tests, strengthened config test assertions. 81 tests passing.
- [x] Fix loop after verification failure — Three components: scope circuit breaker (`Magnitude` struct, `git diff --numstat` check, 3x threshold), leaf fix loop (retry/escalate with `fix_leaf` agent call, reuses model tier progression), branch fix loop (3 rounds Sonnet + 1 Opus for root, `design_fix_subtasks` agent call, fix subtasks marked `is_fix_task` to prevent recursive fix chains). Extracted `fail_task`, `complete_task_verified`, `create_subtasks` helpers and `evaluate_scope` pure function. 18 new tests (81 total).
- [x] Recovery re-decomposition — When a child fails in a branch, Opus recovery agent creates new subtasks. Two approaches: incremental (preserve completed work, append recovery subtasks) or full (skip remaining pending siblings, replace with recovery subtasks). Max 2 recovery rounds per branch. Fix tasks skip recovery (prevents recursive chains). 10 new tests (91 total).
- [x] Checkpoint adjust/escalate actions — Checkpoint decision (proceed/adjust/escalate) now acted upon. Adjust accumulates guidance on parent task (newline-separated), propagated to pending siblings via prompt context. Escalate clears stale guidance, triggers recovery with actual discoveries. Checkpoint uses Haiku (classification task). Agent errors treated as Proceed (best-effort, with mock error injection for testing). New `checkpoint_guidance` field on Task, `CheckpointAdjust`/`CheckpointEscalate` events, TUI integration. 11 new tests (105 total).

- [x] Full codebase audit — 95 review cells (81 matrix, 6 cross-cutting, 8 broad-lens). 541 findings: 4 critical, 120 major, 241 minor, 176 note. See [AUDIT.md](AUDIT.md).
- [x] Wire epic.toml to orchestrator — `EpicConfig` loaded from `epic.toml` at startup (falls back to defaults). All hardcoded orchestrator constants (`MAX_DEPTH`, `RETRIES_PER_TIER`, `MAX_RECOVERY_ROUNDS`, `MAX_BRANCH_FIX_ROUNDS`, `MAX_ROOT_FIX_ROUNDS`) replaced with `LimitsConfig` fields. `FlickAgent` takes `ModelConfig` and `Vec<VerificationStep>` as constructor params. `resolve_model_name` maps `Model` tiers to config-specified names. Verification prompts include configured commands. Zero-value limits clamped to 1. Two review passes: fixed critical bug (default model names were not valid API IDs), collapsed `_with_models` duplication (removed 7 wrapper functions), simplified Orchestrator to hold `LimitsConfig` not full `EpicConfig`, replaced FlickAgent builders with constructor params. 10 new tests (120 total).

## Deferred Items

No remaining deferred items.

## Design Choices (not deferred — intentional constraints)

### Sequential execution only

Epic executes subtasks sequentially by design. This is not a deferral — it is a deliberate constraint that will remain until explicitly reconsidered.

**Rationale (EPIC_DESIGN2):** Simplifies implementation, keeps TUI output and logging coherent, and prioritizes cost control and correctness over throughput while the design matures.

### No multi-language special handling

Epic uses generalized prompts that work across languages. No language-specific logic.

### No git hosting integration

No GitHub/GitLab PR creation, issue tracking, or similar integrations in v1.

## Next Work Candidates

Prioritized from audit findings (see [AUDIT.md](AUDIT.md#recommended-action-items-priority-order)):

1. **Sandboxing** — Two-layer approach documented in [SANDBOXING.md](SANDBOXING.md). Security: VM/container guidance + startup detection warning. Operational correctness: Frida-based runtime interception (frida-gum/frida-core) to enforce per-phase access policies. Next step: prototype Frida Rust bindings. (Critical: U5-R2 #1, U2-R2 #1)
2. ~~**Wire epic.toml to the orchestrator**~~ — **Done.** Config loaded at startup, hardcoded constants replaced. (Major: U10-R1, U7-R1, X6)
3. ~~**Fix model selection to match AGENT_DESIGN.md**~~ — **Done.** Assessment→Haiku, decompose→assessment-selected, recovery→Opus, verification→spec-compliant capping. (Major: U1-R7, U2-R7, U6-R7)
4. ~~**Cap total task count and recovery depth**~~ — **Done.** `max_total_tasks` config (default 100), limit checked at decomposition/fix/recovery. Recovery subtasks inherit parent's `recovery_rounds`. (Major: B7, U1-R2)
5. **Update stale documentation** — 14+ subprocess/Flick references not updated after library migration. (Major: U17-R4, U17-R8)
6. **Add CI pipeline** — No CI exists. Pin Flick dependency. Add `rust-toolchain.toml`. (Critical: X4)
7. **Extract main() into testable function** — Replace `process::exit` with `bail!`, enable integration testing. (Critical: U12-R6)
8. **Remove dead modules** — `git.rs`, `metrics.rs`, `services/*.rs` are empty stubs. (Major: U14-R4, U15-R4, U16-R4)
9. **Deduplicate retry/escalation loop** — `execute_leaf` and `leaf_fix_loop` share ~120 lines of identical code. (Major: B1, U1-R5)
10. **Add cycle detection to `dfs_order`** — Infinite loop on corrupted state files. (Major: U8-R1, U8-R2)

## Decisions Made

### 2026-03-05: Cap total task count and recovery depth

**Scope:** Prevent exponential cost growth from unbounded task creation and nested recovery. 14 files modified (8 source, 6 docs), 10 new tests (135 total). Three review passes applied correctness, simplicity, testing, and documentation fixes.

**Total task cap:** New `max_total_tasks: u32` field in `LimitsConfig` (default: 100, clamped to min 1). `EpicState::task_count()` exposes current count. `check_task_limit()` helper checks before all three task creation points (decomposition, fix subtasks, recovery subtasks). When limit is hit, the branch fails gracefully with a clear error message. New `TaskLimitReached { task_id }` event emitted and handled in TUI worklog. `epic init` prompts for the value.

**Recovery depth inheritance:** Recovery subtasks inherit the parent's `recovery_rounds` counter via `create_subtasks(..., inherit_recovery_rounds: Option<u32>)` parameter. Checkpoint saved atomically with inheritance. Prevents nested recovery branches from each independently using the full `max_recovery_rounds` budget.

**Tests (10 new, 135 total):** `default_max_total_tasks`, `max_total_tasks_round_trips`, `task_limit_blocks_decomposition` (includes event emission check), `task_limit_blocks_fix_subtasks`, `task_limit_blocks_recovery_subtasks`, `recovery_depth_inherited_not_fresh`, `max_total_tasks_zero_clamped_blocks_decomposition`, `task_limit_exact_boundary_permits`, `task_count_tracks_insertions`, `recovery_inherited_budget_blocks_second_recovery`.

### 2026-03-05: Fix model selection to match AGENT_DESIGN.md

**Scope:** All agent call sites now use the model specified in AGENT_DESIGN.md. 7 files modified, 125 tests passing. Two review passes applied simplification and test coverage fixes.

**Model enum (`task/mod.rs`):** Added `#[derive(PartialOrd, Ord)]` (variant order: Haiku < Sonnet < Opus). Removed dead `model: Option<Model>` field from `Task` (only `current_model` is used). Unit test guards ordering invariant.

**AgentService trait (`agent/mod.rs`):** `design_and_decompose()` and `verify()` now take a `model: Model` parameter so the orchestrator can pass the correct model per-spec.

**FlickAgent (`agent/flick.rs`):**
- Assessment: `Model::Sonnet` → `Model::Haiku` (spec: classification task)
- `design_and_decompose`: accepts `model` parameter instead of hardcoding Sonnet (spec: assessment-selected)
- `verify`: accepts `model` parameter instead of hardcoding Sonnet (spec varies by leaf/branch)
- `assess_recovery`: `Model::Sonnet` → `Model::Opus` (spec: requires strongest reasoning)

**Orchestrator (`orchestrator.rs`):** New `verification_model()` helper uses `impl_model.clamp(Haiku, Sonnet)` for leaves, Sonnet for branches. All `verify` and `design_and_decompose` call sites pass the correct model. `MockAgentService` captures model params via `verify_models`/`decompose_models` vectors for test assertions.

**Cleanup:** Deleted `agent/models.rs` — moved `default_max_tokens()` into `config_gen.rs`, eliminated redundant `flick_model_id()`. Removed tautological test, added `default_max_tokens_per_tier` test.

**Tests (7 new, 125 total):** `model_ordering_haiku_lt_sonnet_lt_opus`, `verify_model_leaf_haiku`, `verify_model_leaf_sonnet`, `verify_model_leaf_opus_capped`, `verify_model_branch_always_sonnet`, `decompose_model_from_assessment`, `default_max_tokens_per_tier`.

### 2026-03-05: Wire epic.toml to orchestrator runtime

**Scope:** `epic.toml` config loaded at startup and threaded through orchestrator and agent layer. 7 files modified, 10 new tests (120 total). Two review passes applied fixes.

**Config loading (`main.rs`):** Reads `epic.toml` from project root. Falls back to `EpicConfig::default()` if missing. Passes `ModelConfig` and `Vec<VerificationStep>` to `FlickAgent` as constructor params. Passes `LimitsConfig` to `Orchestrator` via `.with_limits()`.

**Orchestrator (`orchestrator.rs`):** Removed all 5 hardcoded constants (`MAX_DEPTH`, `RETRIES_PER_TIER`, `MAX_RECOVERY_ROUNDS`, `MAX_BRANCH_FIX_ROUNDS`, `MAX_ROOT_FIX_ROUNDS`). Replaced with `self.limits.*` fields. Zero-value limits clamped to minimum 1 at start of `run()`.

**Agent layer (`flick.rs`, `config_gen.rs`, `prompts.rs`):** `FlickAgent` takes `ModelConfig` and `Vec<VerificationStep>` as constructor params (no builder methods). `resolve_model_name(&ModelConfig, Model) -> &str` maps tiers to config-specified names. Verification prompts include configured commands.

**Review pass 1 — critical bug fix:** `ModelConfig::default()` values were short names (`"haiku-4.5"`) not valid API model IDs (`"claude-haiku-4-5-20251001"`). Fixed defaults to match `flick_model_id()`. Added regression test.

**Review pass 2 — simplification:** Removed 7 `#[allow(dead_code)]` wrapper functions (collapsed `_with_models` duplication). Changed `resolve_model_name` from `Option<&ModelConfig>` to `&ModelConfig` (dead `None` branch removed). Orchestrator holds `LimitsConfig` not `EpicConfig`. FlickAgent uses constructor params not builders. Added tests for `branch_fix_rounds`, `root_fix_rounds`, `build_init_config` model threading, and `retry_budget=0` clamping.

**Backward compatibility:** No `epic.toml` required. All defaults match previous hardcoded values.

### 2026-03-05: Sandboxing approach decided

**Two distinct concerns, two distinct solutions:**

1. **Security isolation** — Not epic's job. The only robust boundary is a user-managed VM/container. Epic will: (a) best-effort detect at startup whether it's running in a container/VM, (b) warn if not, (c) provide documentation with recommended container configurations. Epic will NOT implement OS-level sandboxing (namespaces, seccomp, chroot, etc.) and will NOT refuse to run outside a container.

2. **Correct epic operation** — Frida-based runtime interception. frida-gum for in-process syscall hooking (file open/write/exec), frida-core for child process attachment (bash-spawned subprocesses). Per-phase access policies (read set, write set, spawn rules) enforced at the syscall level. Existing enforcement layers retained (ToolGrant bitflags, safe_path containment). Rollout plan: audit mode first (log violations), then enforcement mode (block violations).

**Open questions:** Child process injection latency, write set derivation granularity, network policy, tokio thread pool interaction with per-thread interceptors, graceful degradation if Frida unavailable.

See [SANDBOXING.md](SANDBOXING.md) for full design document.

### 2026-03-05: Full codebase audit completed

**Scope:** 95 independent review agents audited the entire codebase (~9,400 lines of Rust across 29 source files plus 13 design documents). Reviews covered correctness, security, error handling, dead code, simplification, testability, design intent, and doc consistency.

**Results:** 541 findings total — 4 critical, 120 major, 241 minor, 176 note. Detailed reports in `docs/audit/`. Consolidated results and prioritized action items in `docs/AUDIT.md`.

**Top concerns identified:**
1. Unsandboxed bash tool execution (critical security gap)
2. `epic.toml` config collected but never loaded at runtime (all limits hardcoded)
3. Model selection diverges from AGENT_DESIGN.md spec across 4+ call sites
4. Recovery subtasks get fresh budgets, enabling exponential cost growth
5. 14+ stale documentation references to pre-migration subprocess pattern

### 2026-03-05: Leaf retry counter persistence and checkpoint implemented

**Scope:** Two deferred polish items resolved, plus two review passes (correctness fix, simplification). 1 file modified, 5 new tests (110 total).

**Leaf retry counter persistence:** `execute_leaf` now initializes `retries_at_tier` from persisted `attempts` (counting consecutive trailing attempts at the current model tier), matching the pattern already used in `leaf_fix_loop`. On resume, a leaf that used 2 of 3 retries no longer gets a fresh counter.

**Leaf retry checkpoint:** `execute_leaf` now calls `checkpoint_save()` after recording each attempt. On crash mid-retry, persisted attempts survive and the retry counter correctly resumes.

**Top-of-loop escalation guard (review pass):** Both `execute_leaf` and `leaf_fix_loop` now check `retries_at_tier >= RETRIES_PER_TIER` at the top of the loop, before calling the agent. Fixes an edge case where a crash after recording the Nth failure but before escalation would cause one extra attempt at the exhausted tier on resume. Pre-loop `state.get()` calls collapsed from two to one in both methods.

**Review pass — pre-loop drain (simplification):** Extracted the top-of-loop escalation guard into a `while` loop before the main loop in both methods. Eliminates duplicated escalation logic. `execute_leaf` now uses a local `current_model` variable (matching `leaf_fix_loop`'s `fix_model` pattern) instead of re-reading from state each iteration.

**Tests (5 new, 110 total):** `leaf_retry_counter_persists_on_resume` (resume with 2 Haiku failures → escalates after 1 more), `leaf_retry_counter_resume_at_sonnet_tier` (resume at Sonnet tier with mixed Haiku+Sonnet attempts → counts only trailing Sonnet), `leaf_retry_resume_escalates_immediately_when_tier_exhausted` (3 Haiku failures persisted, current_model still Haiku → escalates without extra attempt), `leaf_fix_resume_escalates_immediately_when_tier_exhausted` (same scenario for fix loop), `leaf_retry_attempts_persisted_to_disk` (state file contains attempts after fail+succeed).

### 2026-03-05: Checkpoint adjust/escalate actions implemented

**Scope:** Checkpoint decision (proceed/adjust/escalate) now acted upon instead of discarded. 7 files modified, 11 new tests (105 total).

**Three-way branch in `execute_branch`:**
- **Proceed**: No change (existing behavior).
- **Adjust**: Accumulate guidance string on parent task's `checkpoint_guidance` field (newline-separated if multiple Adjusts). Checkpoint saved. Subsequent siblings see guidance via `TaskContext.checkpoint_guidance` → injected into prompt as `## Checkpoint Guidance` section.
- **Escalate**: Clear stale `checkpoint_guidance`, trigger `attempt_recovery()` with reason including actual discoveries. Reuses full recovery machinery (assess → design → create subtasks). If recovery fails, propagates failure. If recovery succeeds, restarts child loop.

**Error handling:** Agent errors in checkpoint classification treated as Proceed (best-effort, matching recovery error handling pattern). Warning logged to stderr. Mock error injection via `checkpoint_errors` queue enables real error path testing.

**Task changes:** New `checkpoint_guidance: Option<String>` field on `Task` (persisted, serde).

**Context changes:** New `checkpoint_guidance: Option<String>` field on `TaskContext`. Populated from parent task in `build_context()`. `format_context()` in prompts.rs appends `## Checkpoint Guidance` section when present.

**New events:** `CheckpointAdjust { task_id }`, `CheckpointEscalate { task_id }`. TUI worklog handles both.

**Model change:** Checkpoint uses `Model::Haiku` (was Sonnet), matching AGENT_DESIGN.md spec for classification tasks.

**Tests (11 new):** `context_format_includes_checkpoint_guidance` (prompt), `checkpoint_adjust_stores_guidance` (adjust path, event, guidance stored), `checkpoint_escalate_triggers_recovery` (escalate → recovery → success), `checkpoint_escalate_unrecoverable_fails` (escalate → unrecoverable → failure), `checkpoint_agent_error_treated_as_proceed` (real error injection via mock), `checkpoint_guidance_persisted` (JSON round-trip), `checkpoint_multiple_adjusts_accumulates_guidance` (guidance accumulation), `checkpoint_escalate_clears_prior_guidance` (escalate clears stale guidance), `checkpoint_escalate_on_fix_task_fails` (fix task rejects recovery), `checkpoint_escalate_recovery_rounds_exhausted` (exhausted budget), `checkpoint_guidance_flows_to_child_context` (build_context propagation).

### 2026-03-05: Recovery re-decomposition implemented

**Scope:** When a child task fails in a branch, the orchestrator now invokes an Opus recovery agent to create recovery subtasks. 8 files modified, 10 new tests (91 total).

**Two recovery approaches:**
- **Incremental**: Preserve completed work. Recovery subtasks appended to parent's subtask list. Remaining pending siblings still execute after recovery subtasks.
- **Full re-decomposition**: Remaining pending siblings marked as Failed ("superseded by recovery re-decomposition"). Only recovery subtasks execute.

**Recovery flow:** Child fails → check recovery budget (max 2 rounds) and not a fix task → `assess_recovery()` (Opus, no tools) determines if recoverable and suggests strategy → `design_recovery_subtasks()` (Opus, with tools) creates recovery plan with fresh magnitude estimates → subtasks created and appended → child loop restarts.

**New types:** `RecoveryPlan` (full_redecomposition, subtasks, rationale), `RecoveryPlanWire` with `TryFrom` conversion, `recovery_plan_schema()`, `build_recovery_plan_config()`.

**New trait method:** `AgentService::design_recovery_subtasks(ctx, failure_reason, strategy, recovery_round) → RecoveryPlan`. Uses Opus model with decompose-phase tools.

**New events:** `RecoveryStarted { task_id, round }`, `RecoveryPlanSelected { task_id, approach }`, `RecoverySubtasksCreated { task_id, count, round }`.

**Task changes:** `recovery_rounds: u32` field on `Task` (persisted, default 0).

**Orchestrator changes:** `execute_branch()` rewritten with outer recovery loop. New `attempt_recovery()` method. `create_subtasks_inner()` extracted to separate `mark_fix` from `append` behavior. `MAX_RECOVERY_ROUNDS = 2` constant.

**Guard rails:** Fix tasks (`is_fix_task = true`) skip recovery entirely — prevents recursive recovery chains. Empty recovery plan treated as round failure. Recovery subtasks are NOT marked as fix tasks — they use the full pipeline including fix loops.

**Tests (13 new):** `recovery_incremental_creates_subtasks`, `recovery_full_redecomposition_skips_pending`, `recovery_round_limit_exhausted`, `recovery_not_attempted_for_fix_tasks`, `recovery_not_attempted_when_unrecoverable`, `recovery_rounds_persisted`, `recovery_empty_plan_fails`, `recovery_full_redecomp_preserves_completed_siblings`, `recovery_emits_events`, `recovery_plan_wire_incremental`, `recovery_plan_wire_full`, `recovery_plan_wire_invalid_approach`, `design_recovery_subtasks_prompt_contains_context`.

**Review pass:** Fixed checkpoint ordering (increment `recovery_rounds` before subtask creation to prevent extra round on crash-resume). Agent errors in `assess_recovery`/`design_recovery_subtasks` now treated as failed recovery (logged + returns Failed) instead of aborting the run. Collapsed `create_subtasks`/`create_subtasks_inner` into single method with explicit `(mark_fix, append)` parameters. Extracted shared `parse_subtask_wire()` function to deduplicate magnitude parsing between `DecompositionWire` and `RecoveryPlanWire`. Replaced `From<RecoveryWire> for Option<String>` with `RecoveryWire::into_strategy()`. Added 3 tests, strengthened existing test assertions. 94 tests total.

### 2026-03-05: Flick library migration implemented

**Scope:** Replaced subprocess invocation with direct library calls. 6 files modified, 0 new files.

**Dependency change:** Added `flick` as git dependency (`flick = { git = "https://github.com/bitmonk8/flick" }`). Removed `serde_yaml`. Net new transitive dependencies: reqwest, serde_yml, chacha20poly1305, zeroize, hex, xxhash-rust.

**FlickAgent rewrite (`flick.rs`):** Removed `FlickOutput`, `ContentBlock`, `UsageSummary`, `FlickError`, `ToolResultEntry` local types — replaced by `flick::FlickResult`, `flick::ContentBlock`, etc. Removed `invoke_flick()` (subprocess spawning), `format_exit_status()`. `FlickAgent` fields: removed `flick_path`, `work_dir`; constructor is now `const fn`, no longer async. New `build_client()` method calls `resolve_provider()` + `FlickClient::new()`. `run_structured()` and `run_with_tools()` use `FlickClient::run()`/`resume()` with `flick::Context`. Tool results passed as `Vec<ContentBlock::ToolResult>` — no file I/O.

**Config generation (`config_gen.rs`):** Removed `FlickConfig`, `FlickModelConfig`, `FlickProviderConfig` structs. Removed `write_config()` async function. New `build_config()` helper builds JSON in-memory, parses via `Config::from_str(json, ConfigFormat::Json)`. Config builder functions return `Result<flick::Config>` instead of `FlickConfig`. Parameters changed from `String` to `&str` (clippy: `needless_pass_by_value`).

**Tools (`tools.rs`):** Renamed `FlickToolDef.input_schema` to `parameters` to match Flick's `ToolConfig` field name.

**CLI (`cli.rs`):** Removed `--flick-path` global option and `EPIC_FLICK_PATH` env var.

**Main (`main.rs`):** Removed `flick_path` and `work_dir` wiring. `FlickAgent::new()` call simplified (3 args instead of 5, no `.await`).

**Tests:** 78 passing (3 subprocess-specific tests removed: `config_serializes_to_yaml`, `write_config_creates_file`, `tool_result_entry_serializes`). 5 new/rewritten tests: `extract_text_from_result`, `extract_text_missing`, `extract_tool_calls_from_result`, `check_error_on_error_status`, `check_error_on_complete`. 0 new clippy warnings.

### 2026-03-05: Fix loop after verification failure implemented

**Scope:** Three components across 7 files, 18 new tests (81 total). Review pass added extracted helpers, correctness fixes, and cruft cleanup.

**Scope circuit breaker:** `Magnitude` struct (max_lines_added/modified/deleted) on `Task`. `check_scope_circuit_breaker()` runs `git diff --numstat HEAD`, compares against 3x magnitude estimate. Best-effort (skipped if no magnitude, no project root, or git fails). `evaluate_scope()` extracted as pure function for testability. `AssessmentWire` extended with optional magnitude fields.

**Leaf fix loop:** `fix_leaf` added to `AgentService` trait. `leaf_fix_loop()` in orchestrator: retry/escalate with model tier progression (starting at the model that wrote the code). Scope check before each attempt. Fix attempts tracked separately in `task.fix_attempts`. Resume correctness: `fix_retries_at_tier` initialized from persisted `fix_attempts`. New events: `FixAttempt`, `FixModelEscalated`.

**Branch fix loop:** `design_fix_subtasks` added to `AgentService` trait. `branch_fix_loop()` in orchestrator: max 3 rounds (Sonnet) for non-root, 4 rounds (3 Sonnet + 1 Opus) for root. Each round creates fix subtasks (marked `is_fix_task = true`), executes them through full pipeline, then re-verifies. Fix subtasks CAN use leaf fix loop but CANNOT trigger branch fix loop (prevents recursive chains). Empty subtask list handled as round failure. New events: `BranchFixRound`, `FixSubtasksCreated`. New fields on Task: `verification_fix_rounds`, `is_fix_task`.

**Helpers extracted:** `fail_task()`, `complete_task_verified()`, `create_subtasks()` reduce duplication across orchestrator methods (~90 lines). Removed stale `#[allow(dead_code)]` annotations and unused `branch_fix_rounds` config field.

### 2026-02-25: Configuration format — TOML

**Decision:** TOML for all Epic configuration files (`epic.toml`, `.epic/config.toml`).

**Rationale:** Rust ecosystem standard. Shallow config fits naturally. `toml` crate is mature and serde-native. YAML rejected (archived crate, implicit type coercion). RON rejected (too niche).

### 2026-02-25: `epic init` — agent-driven interactive scaffolding

**Decision:** `epic init` uses an agent to explore the project (build system markers, test frameworks, linters, CI config), presents findings, and interactively confirms verification steps. Writes `epic.toml`.

### 2026-02-25: Batch decisions — Rust, Scope, Document Store, TUI

**Rust-specific:** tokio (ecosystem standard), anyhow + thiserror, serde + serde_json + toml.

**Scope (v1 boundaries):** Sequential execution (deliberate design choice, not deferral — see "Design Choices" section), no multi-language special handling, no git hosting integration.

**Document Store:** File-based (markdown) for v1. Librarian via Flick agent (Haiku, read-only tools).

**TUI:** ratatui + crossterm. Read-only monitoring for v1.

**Config ownership:** Epic owns all config. `[agent]` section in epic.toml for runtime knobs.

### 2026-02-26: Core orchestrator loop implemented

**Scope:** Task types, assessment/branch/verify data structs, `Event` enum with unbounded channel, `EpicState` with JSON persistence, `AgentService` trait (6 async methods), `Orchestrator<A: AgentService>` with DFS loop, retry/escalation (3 retries per tier, Haiku→Sonnet→Opus).

**Tests (6):** single_leaf, two_children, leaf_retry_and_escalation, terminal_failure, depth_cap_forces_leaf, persistence_round_trip.

**Previously deferred:** Checkpoint adjust/escalate actions (since implemented), fix loop (since implemented), recovery re-decomposition (since implemented), leaf retry counter persistence (since implemented), leaf retry checkpoint (since implemented). All deferred items resolved.

### 2026-03-04: Replace ZeroClaw with Flick

**Decision:** Replace ZeroClaw (library dependency via forked submodule) with [Flick](https://github.com/bitmonk8/flick) (initially as external executable, later migrated to library crate — see 2026-03-05 Flick library migration decision).

**Rationale:** ZeroClaw had provenance concerns (star farming, 12-day-old project, crypto fraud ecosystem), heavy transitive dependency tree (~771 packages), fork maintenance burden, and intermittent compiler ICE.

### 2026-03-04: Agent call wiring implemented

**Scope:** `FlickAgent` implementing `AgentService` trait. 8 files modified/created:
- `models.rs` — Model → Flick model ID mapping and token limits (later collapsed into `config_gen.rs`)
- `tools.rs` — `ToolGrant` bitflags, tool definitions for Flick config, stubbed `execute_tool()`
- `config_gen.rs` — `FlickConfig` YAML generation, 6 wire format types (`AssessmentWire`, `DecompositionWire`, etc.) with `TryFrom` conversions, 6 JSON output schemas, `write_config()`
- `prompts.rs` — Prompt assembly (system prompt + query) for all 6 `AgentService` methods
- `flick.rs` — `FlickAgent` struct with `invoke_flick()`, `run_structured()`, `run_with_tools()` (tool loop with 50-round cap, `kill_on_drop`, timeout handling), full `AgentService` impl
- `main.rs` — Wires `FlickAgent` into `Orchestrator` with env-var config

**Tests:** 32 new unit tests (38 total). 0 clippy warnings.

**Deferred:** Tool execution (stubbed). Discoveries propagation deferred (required trait change) — implemented in later milestone.

### 2026-03-04: Tool execution implemented

**Scope:** All 6 tools implemented in `tools.rs`: `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `bash`. `flick.rs` call site updated from sync to async.

**Security:** Path sandboxing via `safe_path()` (canonicalization + containment check), `verify_ancestors_within_root()` for write_file, symlink guard on `allow_new_file` paths, `follow_links(false)` on directory walks.

**Resource limits:** `read_file` streams first 256 KiB only (no full-file load), `grep` skips files >10 MiB, `glob` caps at 1000 results, `grep` caps output at 64 KiB, `bash` output capped at 64 KiB with char-boundary-safe truncation, bash timeout clamped to 600s max.

**Async:** `glob`/`grep` use `spawn_blocking` to avoid blocking the tokio runtime with sync `WalkDir`/`std::fs` I/O.

**Tests:** 15 new tests (53 total). 0 clippy warnings.

### 2026-03-05: State persistence integration

**Scope:** `EpicState` gains `root_id` field for session resume. `Orchestrator` gains `state_path: Option<PathBuf>` and `checkpoint_save()` method. `main.rs` loads from `.epic/state.json` if present, otherwise creates fresh state. Extracted `finalize_task` method for verification/completion logic.

**Checkpoint locations:** After assessment (path/model applied), after decomposition (subtasks created), after each task completion/failure (verification pass/fail, execution failure).

**Resume semantics:** `execute_task` returns early for `Completed`/`Failed` tasks. `Verifying` tasks skip to re-verification (not re-execution). `Executing` tasks with `path` already set skip re-assessment. `execute_branch` reuses existing `subtask_ids` (skips `design_and_decompose`), skips terminal children. `Orchestrator::run()` sets `root_id` on state.

**Atomic writes:** `save()` writes to `.json.tmp` then renames. Prevents corruption on kill.

**Goal mismatch:** CLI goal compared against persisted root goal; exits with diagnostic on mismatch.

**Corrupt state:** `EpicState::load()` errors produce human-readable message and exit, not raw serde trace.

**Best-effort checkpoints:** `checkpoint_save()` logs errors to stderr but does not abort the run.

**Previously deferred:** Leaf retry counter persistence and leaf retry checkpoint — both implemented (see 2026-03-05 decision).

**Tests:** 5 new tests: `checkpoint_saves_state`, `resume_skips_completed_child`, `resume_skips_decomposition_when_subtasks_exist`, `resume_mid_execution_branch_not_reassessed`, `resume_verifying_skips_execution` (58 total). 0 clippy warnings.

### 2026-03-05: TUI event consumer implemented

**Scope:** `TuiApp` in `tui/mod.rs` consumes `EventReceiver` and renders via ratatui + crossterm. 4 files implemented/rewritten, 2 files modified.

**Layout:** Header (status, goal, progress counter, elapsed time), body (task tree + worklog, optionally + metrics), footer (keybindings).

**Task tree:** DFS-ordered display built from `TaskRegistered` events. Status indicators: ✓ completed, ✗ failed, ▸ in progress, · pending. Current-task cursor (←). Scrollable via ↑↓.

**Worklog:** Timestamped event stream with color-coded entries (green=success, red=error, yellow=warn). Follow-tail toggle (t key).

**Metrics panel:** Toggle (m key). Shows task counts by phase (total, completed, in-progress, pending, failed).

**Event additions:** `TaskRegistered { task_id, parent_id, goal, depth }` added to `Event` enum. Emitted by orchestrator for root task, pre-existing subtasks (resume), and newly created subtasks.

**Architecture:** Orchestrator runs in background `tokio::spawn` task. TUI runs in `spawn_blocking` (not on async runtime). Events drained non-blockingly via `try_recv`. `EPIC_NO_TUI=1` env var for headless mode (original behavior). On TUI quit, orchestrator gets 2s grace period then `abort_handle.abort()`.

**Safety:** Panic hook restores terminal (raw mode + alternate screen) before default handler. UTF-8 safe truncation via `char_indices().take_while()` on full string. Scroll clamped in both key handler and render. Worklog capped at 10,000 entries. `TaskCompleted` defensively sets phase regardless of event ordering. `TaskPhase` derives `Copy`.

**Files modified:** `events.rs` (new variant), `orchestrator.rs` (emit `TaskRegistered`, `Copy` cleanup), `main.rs` (TUI/headless mode split, `spawn_blocking`, abort), `task/mod.rs` (`TaskPhase` + `Copy`), `tui/mod.rs`, `tui/task_tree.rs`, `tui/worklog.rs`, `tui/metrics.rs`.

**Tests:** 58 passing (no new tests — TUI is UI code, tested via existing orchestrator event emission tests). 0 clippy warnings.

### 2026-03-05: Discoveries propagation implemented

**Scope:** Leaf execution results now carry discoveries alongside the task outcome. `AgentService::execute_leaf` returns `LeafResult` (outcome + discoveries) instead of bare `TaskOutcome`. Orchestrator stores discoveries on the task and triggers checkpoint flow for sibling coordination.

**Changes:**
- `task/mod.rs` — Added `LeafResult` struct (outcome + discoveries)
- `agent/mod.rs` — `execute_leaf` return type changed to `LeafResult`
- `agent/config_gen.rs` — `TryFrom<TaskOutcomeWire>` now produces `LeafResult`, extracting discoveries from wire format
- `agent/flick.rs` — `FlickAgent::execute_leaf` returns `LeafResult`
- `orchestrator.rs` — `execute_leaf` destructures `LeafResult`, stores discoveries on task, emits `DiscoveriesRecorded` event
- `events.rs` — Added `DiscoveriesRecorded { task_id, count }` variant
- `tui/mod.rs` — Handles `DiscoveriesRecorded` in worklog

**Data flow:** Agent returns `TaskOutcomeWire` with optional `discoveries` → `TryFrom` extracts into `LeafResult` → orchestrator stores on `Task.discoveries` → `execute_branch` reads child discoveries → calls `checkpoint()` with them → sibling context includes discoveries via `SiblingSummary` → prompt formatting shows discoveries to subsequent tasks.

**Tests:** 2 new tests: `task_outcome_wire_with_discoveries` (wire conversion), `discoveries_propagated_to_checkpoint` (end-to-end: leaf reports discoveries → stored on task → checkpoint called → event emitted). 60 total. 0 clippy warnings.

### 2026-03-05: CLI (clap) implemented

**Scope:** Replaced ad-hoc `std::env::args()` + env-var parsing with `clap` derive-based CLI.

**Subcommands:**
- `epic run <goal>` — Start a new run. If state file exists with same goal, resumes transparently. Different goal prints diagnostic and exits.
- `epic resume` — Resume from `.epic/state.json`. Exits with message if no state file found.
- `epic status` — Prints goal, root phase, and task counts (completed/in-progress/pending/failed) from persisted state. No agent or Flick needed.

**Global options:** `--credential` (env: `EPIC_CREDENTIAL`, default: `anthropic`), `--no-tui` (env: `EPIC_NO_TUI`). (Note: `--flick-path` was originally listed here but removed in the Flick library migration.)

**Files:** New `cli.rs` module. `main.rs` rewritten. `Cargo.toml` added `env` feature to clap.

**Tests:** 60 passing (no new tests — CLI is integration surface). 0 clippy warnings.

### 2026-03-05: `epic init` implemented

**Scope:** Agent-driven interactive configuration scaffolding. New `Init` subcommand. 3 files created, 5 files modified.

**Flow:**
1. Early `epic.toml` existence check (before Flick agent construction)
2. `FlickAgent::explore_for_init()` — Sonnet agent with read-only tools scans project for build system markers, test frameworks, linters, CI config
3. `InitFindingsWire` structured output with `DetectedStepWire` entries
4. Interactive confirmation: accept/edit/skip each step, add custom steps
5. `prompt_models()` — model preference confirmation (accept defaults or customize)
6. `prompt_limits()` — depth/budget limit confirmation (accept defaults or customize)
7. Atomic write (`epic.toml.tmp` → rename → `epic.toml`)
8. Declined steps appended as TOML comments for reference

**Config types:** `EpicConfig` (top-level), `VerificationStep`, `ModelConfig`, `LimitsConfig`, `AgentConfig`, `ProjectConfig` — all serde Serialize/Deserialize with TOML defaults.

**Files created:** `init.rs`, `config/project.rs` (rewritten from stub).
**Files modified:** `cli.rs` (Init subcommand), `main.rs` (wiring), `agent/config_gen.rs` (wire types + schema), `agent/flick.rs` (explore method), `agent/mod.rs` + `config/mod.rs` (visibility).

**Error handling:** `read_line_checked` propagates I/O errors for critical prompts. `read_line_or_eof` propagates I/O errors, treats EOF as empty. Init uses `bail!()` not `process::exit()`.

**Tests:** 3 new tests (63 total): `default_config_round_trips`, `parse_with_verification_steps`, `parse_minimal_config`. 0 clippy warnings in new code.
