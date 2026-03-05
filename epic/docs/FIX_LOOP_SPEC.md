# Fix Loop After Verification Failure — Implementation Spec

## Overview

When verification fails, the current implementation marks the task as immediately failed. This spec adds fix loops: retry mechanisms that attempt to repair verification failures before declaring terminal failure.

Two distinct fix loops exist, corresponding to the two task paths:

- **Leaf fix loop**: Re-execute the leaf agent with verification failure context, using the existing retry/escalation pattern.
- **Branch fix loop**: Create fix subtasks from verification issues, execute them, then re-verify the branch.

Both include a **scope circuit breaker** that halts fix attempts when actual changes exceed the estimated magnitude.

## Source Design

All rules originate from `EPIC_DESIGN2.md` (sections: Retry Budget, Guardrails and Recovery, Branch Task Verification).

---

## Phase 1: Leaf Fix Loop

### Behavior

When a leaf task's verification fails:

1. Record the verification failure reason.
2. Check the scope circuit breaker (see below). If tripped, fail immediately with `SCOPE_EXCEEDED`.
3. Call `fix_leaf()` — the agent re-executes with knowledge of what failed.
4. Re-verify.
5. On pass: complete the task.
6. On fail: increment `fix_retries_at_tier`.
   - If `fix_retries_at_tier < 3`: loop to step 2.
   - If `fix_retries_at_tier == 3`: escalate to next model tier, reset counter, loop to step 2.
   - If current tier is Opus and retries exhausted: terminal failure.

### Model Selection

The fix loop uses the same model tier progression as initial execution: Haiku -> Sonnet -> Opus.

The starting tier for fix attempts is the model that produced the initial (failing) output. Rationale: if Sonnet wrote the code, Haiku is unlikely to fix it.

### AgentService Changes

New method:

```rust
async fn fix_leaf(
    &self,
    ctx: &TaskContext,
    model: Model,
    failure_reason: &str,
    attempt: u32,
) -> Result<LeafResult>;
```

Semantics: same as `execute_leaf` but the prompt includes the verification failure reason and instructs the agent to fix the issues rather than start from scratch. Returns `LeafResult` (outcome + discoveries), reusing the existing wire type (`TaskOutcomeWire`).

### Orchestrator Changes

`finalize_task()` currently:
```
verify() -> Pass -> Completed
          -> Fail -> Failed (immediate)
```

Becomes:
```
verify() -> Pass -> Completed
          -> Fail -> if leaf: enter leaf fix loop
                     if branch: enter branch fix loop (Phase 2)
```

New method `leaf_fix_loop(&mut self, id: TaskId, initial_failure: &str) -> Result<TaskOutcome>`:

- Tracks `fix_retries_at_tier: u32` and `fix_model: Model` (starting at the model that executed the leaf).
- Loop: scope check -> fix_leaf() -> verify() -> pass/fail/retry/escalate.
- Emits events at each step.
- Checkpoints after each fix attempt.

### Task State Changes

Add to `Task`:
```rust
pub fix_attempts: Vec<Attempt>,
```

Reuses existing `Attempt` struct. Tracks fix-specific attempts separately from initial execution attempts. Persisted for resume.

On resume: if a task is in `Verifying` phase with `fix_attempts.len() > 0`, the fix loop resumes with the correct retry counter.

### Wire Types

No new wire types. `fix_leaf` reuses `TaskOutcomeWire` and the existing schema. The differentiation is in the prompt, not the response format.

### Prompt Assembly

New function `build_fix_leaf_prompt(ctx, failure_reason, attempt)`:

- System prompt: same workspace/tools context as `execute_leaf`.
- Query: includes the original goal, the verification failure reason, the attempt number, and instructions to fix the specific issues rather than rewrite from scratch.

### Events

New variants:

```rust
FixAttempt { task_id: TaskId, attempt: u32, model: Model }
FixModelEscalated { task_id: TaskId, from: Model, to: Model }
```

`VerificationComplete` (existing) is emitted after each re-verification. `TaskCompleted` (existing) is emitted on terminal pass or fail.

### Estimated Scope

- `orchestrator.rs`: +50-70 lines (new `leaf_fix_loop` method, modify `finalize_task`)
- `agent/mod.rs`: +5 lines (trait method)
- `agent/flick.rs`: +30-40 lines (implement `fix_leaf`)
- `agent/prompts.rs`: +25-35 lines (fix prompt builder)
- `task/mod.rs`: +3 lines (`fix_attempts` field, default)
- `events.rs`: +6 lines (2 new variants)
- `tui/mod.rs`: +10 lines (handle new events in worklog)

Total: ~130-170 lines across 7 files.

---

## Phase 2: Branch Fix Loop

### Behavior

When a branch task's verification fails:

1. Increment `verification_fix_rounds`.
2. Check round budget:
   - Non-root branch: max 3 rounds (Sonnet).
   - Root branch: max 4 rounds (3 Sonnet + 1 Opus).
3. Call `design_fix_subtasks()` — the agent analyzes verification issues and produces fix subtask specs.
4. Create and execute fix subtasks through the normal pipeline (assess -> execute -> verify).
5. Re-verify the branch.
6. On pass: complete the branch.
7. On fail:
   - If rounds remaining: loop to step 1.
   - Non-root, rounds exhausted: fail (escalate to parent recovery, if implemented).
   - Root, rounds exhausted: terminal failure with summary of resolved and remaining issues.

### Branch Verification Content

Each verification round performs three reviews (per EPIC_DESIGN2):
- Correctness: do the changes satisfy the goal?
- Completeness: are all aspects addressed?
- Aggregate simplification: can the combined output be simplified?

Fix subtasks are created to address issues found in any of these three areas.

### Model Selection

- Rounds 1-3: Sonnet (fixed, not escalating within rounds).
- Round 4 (root only): Opus.

### AgentService Changes

New method:

```rust
async fn design_fix_subtasks(
    &self,
    ctx: &TaskContext,
    model: Model,
    verification_issues: &str,
    round: u32,
) -> Result<DecompositionResult>;
```

Returns `DecompositionResult` (reuses existing type). The subtask specs are structurally identical to normal decomposition output — they go through the same execution pipeline.

### Orchestrator Changes

New method `branch_fix_loop(&mut self, id: TaskId, initial_failure: &str) -> Result<TaskOutcome>`:

- Tracks `verification_fix_rounds` on the task.
- Each round: design_fix_subtasks() -> create subtasks -> execute each -> re-verify.
- Fix subtasks are children of the branch, appended to `subtask_ids`.
- Fix subtasks are marked with `is_fix_task: bool` on `Task` for TUI display differentiation.
- Checkpoints after subtask creation and after each subtask completion.

### Task State Changes

Add to `Task`:
```rust
pub verification_fix_rounds: u32,
pub is_fix_task: bool,
```

`verification_fix_rounds` tracks how many fix rounds the branch has attempted. Persisted for resume.

`is_fix_task` marks subtasks created by the fix loop (vs original decomposition). Used for display and to prevent fix subtasks from themselves spawning unbounded fix chains (fix subtasks get the standard verification pass/fail without their own branch fix loop).

### Wire Types

Reuses `DecompositionWire` and existing schema. The fix subtask specs have the same structure as normal subtasks. Differentiation is in the prompt.

### Prompt Assembly

New function `build_design_fix_subtasks_prompt(ctx, verification_issues, round)`:

- System prompt: same context as `design_and_decompose`.
- Query: includes the branch goal, completed subtask summaries, the verification failure details, the round number, and instructions to create targeted fix subtasks.

### Events

New variants:

```rust
BranchFixRound { task_id: TaskId, round: u32, model: Model }
FixSubtasksCreated { task_id: TaskId, count: usize, round: u32 }
```

### Fix Subtask Execution Rules

- Fix subtasks go through the full pipeline: assess -> execute -> verify.
- Fix subtasks CAN use the leaf fix loop (Phase 1) if their own verification fails.
- Fix subtasks CANNOT trigger the branch fix loop. This prevents recursive fix chains.
- Fix subtasks receive context about the original branch goal and what they're fixing.

### Estimated Scope

- `orchestrator.rs`: +80-110 lines (new `branch_fix_loop` method, modify `finalize_task`, fix subtask creation/execution)
- `agent/mod.rs`: +5 lines (trait method)
- `agent/flick.rs`: +30-40 lines (implement `design_fix_subtasks`)
- `agent/prompts.rs`: +40-60 lines (fix prompt builder)
- `task/mod.rs`: +5 lines (`verification_fix_rounds`, `is_fix_task` fields)
- `events.rs`: +6 lines (2 new variants)
- `tui/mod.rs`: +15 lines (handle new events, display fix tasks distinctly)

Total: ~180-250 lines across 7 files.

---

## Scope Circuit Breaker

### Behavior

Before each fix attempt (leaf or branch), measure the actual change magnitude and compare against the parent's estimate.

1. Parent's `assess()` returns magnitude estimates: `max_lines_added`, `max_lines_modified`, `max_lines_deleted`, each with a 50% conservative buffer already applied.
2. Before each fix attempt, run `git diff --numstat` against the task's workspace.
3. If **any metric exceeds 3x the estimate**: immediately fail the task with reason `SCOPE_EXCEEDED`, roll back uncommitted changes.

### Implementation

New function in orchestrator (or a utility module):

```rust
async fn check_scope_circuit_breaker(
    &self,
    task_id: TaskId,
    workspace: &Path,
) -> Result<ScopeCheck>;

enum ScopeCheck {
    WithinBounds,
    Exceeded { metric: String, actual: u64, limit: u64 },
}
```

Uses `git diff --numstat` via `tokio::process::Command`.

### Magnitude Estimates

`AssessmentResult` currently contains `magnitude: Option<Magnitude>`. If `Magnitude` doesn't include line-level estimates, it needs to be extended:

```rust
pub struct Magnitude {
    pub max_lines_added: u64,
    pub max_lines_modified: u64,
    pub max_lines_deleted: u64,
}
```

The 50% buffer is applied by the assessing agent (included in the prompt instructions). The 3x threshold is applied by the orchestrator.

If no magnitude estimate exists (legacy tasks, or assessment didn't produce one), the circuit breaker is skipped for that task.

### Where It Runs

- **Leaf fix loop**: checked before each `fix_leaf()` call.
- **Branch fix loop**: checked before each `design_fix_subtasks()` call.
- **Not checked** on the initial execution attempt (only on fix retries).

### Estimated Scope

- `orchestrator.rs`: +25-35 lines (scope check function, integration into fix loops)
- `task/mod.rs` or `task/assess.rs`: +5-10 lines (Magnitude struct if not already present)
- `agent/config_gen.rs`: +5-10 lines (magnitude fields in AssessmentWire if needed)

Total: ~35-55 lines.

---

## Combined Scope Summary

| Component | Files | Lines |
|---|---|---|
| Leaf fix loop (Phase 1) | 7 | ~130-170 |
| Branch fix loop (Phase 2) | 7 | ~180-250 |
| Scope circuit breaker | 3 | ~35-55 |
| **Total** | **7 unique files** | **~345-475** |

Phases can be implemented and tested independently. Phase 1 is a prerequisite for Phase 2 only in the sense that fix subtasks should be able to use the leaf fix loop.

## Test Plan

### Leaf Fix Loop Tests

- `leaf_fix_passes_on_retry`: verification fails once, fix_leaf succeeds, re-verification passes.
- `leaf_fix_escalates_model`: 3 failures at starting tier, escalates to next model.
- `leaf_fix_terminal_failure`: all tiers exhausted, task fails.
- `leaf_fix_scope_exceeded`: scope circuit breaker trips, immediate failure.
- `leaf_fix_persists_and_resumes`: crash mid-fix-loop, resume continues with correct counter.

### Branch Fix Loop Tests

- `branch_fix_creates_subtasks`: verification fails, fix subtasks created and executed, re-verification passes.
- `branch_fix_round_budget`: non-root exhausts 3 rounds, fails.
- `branch_fix_root_opus_round`: root gets 4th round at Opus.
- `branch_fix_subtasks_no_recursive_fix`: fix subtasks use leaf fix loop but not branch fix loop.

### Scope Circuit Breaker Tests

- `scope_check_within_bounds`: diff within 3x estimate, returns WithinBounds.
- `scope_check_exceeded`: diff exceeds 3x estimate, returns Exceeded.
- `scope_check_skipped_no_magnitude`: no magnitude estimate, circuit breaker skipped.
