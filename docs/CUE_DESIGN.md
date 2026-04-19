# Design

Cue is a generic recursive task orchestration framework. It defines the coordination algorithm, trait contracts, protocol types, and event system for depth-first task execution with retry, escalation, fix loops, and recovery. Cue has no AI, vault, reel, flick, or lot dependencies — application crates (e.g., epic) provide concrete implementations via `TaskNode` and `TaskStore` traits.

Parameterized over `TaskStore: Send`. The orchestrator is domain-agnostic; all task behavior and state management are delegated to trait implementations.

---

## Trait Contracts

### TaskNode

Per-task protocol. Requires `Send`.

**Read accessors** (8 methods):

| Method | Returns | Purpose |
|---|---|---|
| `id()` | `TaskId` | Unique identity |
| `parent_id()` | `Option<TaskId>` | Parent in tree (None for root) |
| `goal()` | `&str` | Task objective |
| `depth()` | `u32` | Tree depth (root = 0) |
| `phase()` | `TaskPhase` | Current state machine phase |
| `subtask_ids()` | `&[TaskId]` | Child task IDs |
| `discoveries()` | `&[String]` | Insights from execution |
| `recovery_rounds()` | `u32` | Recovery attempts made |

**Decision methods** (6 methods):

| Method | Returns | Purpose |
|---|---|---|
| `is_terminal()` | `bool` | Task reached terminal state? |
| `resume_point()` | `ResumePoint` | Where to resume on restart |
| `forced_assessment(max_depth)` | `Option<AssessmentResult>` | Override agent assessment (depth/root forcing) |
| `needs_decomposition()` | `bool` | Branch needs decomposition design? |
| `decompose_model()` | `Model` | Model for decomposition call |
| `registration_info()` | `RegistrationInfo` | Metadata for registration event |

**Mutations** (6 methods):

| Method | Purpose |
|---|---|
| `set_phase(phase)` | Update phase after transition validation |
| `set_assessment(path, model, magnitude)` | Store assessment decision |
| `set_decomposition_rationale(rationale)` | Store decomposition reasoning |
| `set_subtask_ids(ids, append)` | Set or append child IDs |
| `increment_fix_rounds() -> u32` | Increment fix counter, return new value |
| `accumulate_usage(meta) -> f64` | Accumulate usage metrics, return total cost |

**Lifecycle** (8 async methods, all return `impl Future + Send`):

| Method | Signature | Purpose |
|---|---|---|
| `assess` | `(&mut self, &TreeContext) -> Result<AssessmentResult>` | Classify task: leaf/branch, select model |
| `decompose` | `(&mut self, &TreeContext, Model) -> Result<DecompositionResult>` | Design subtask breakdown |
| `execute_leaf` | `(&mut self, &TreeContext) -> TaskOutcome` | Execute leaf task (agent call) |
| `verify_branch` | `(&mut self, &TreeContext) -> Result<BranchVerifyOutcome>` | Verify branch children |
| `fix_round_budget_check` | `(&self, &LimitsConfig) -> FixBudgetCheck` | Check fix loop budget (sync) |
| `check_branch_scope` | `(&self) -> ScopeCheck` | Scope circuit breaker |
| `design_fix` | `(&mut self, &TreeContext, &str, u32, Model) -> Result<Result<Vec<SubtaskSpec>, String>>` | Design fix subtasks for failed verification |
| `handle_checkpoint` | `(&mut self, &TreeContext, &[String]) -> Result<ChildResponse>` | Parent reviews child discoveries |
| `can_attempt_recovery` | `(&self, &LimitsConfig) -> RecoveryEligibility` | Check recovery eligibility (sync) |
| `assess_and_design_recovery` | `(&mut self, &TreeContext, &str, u32) -> Result<RecoveryDecision>` | Design recovery plan |

**Error convention**: `execute_leaf` returns `TaskOutcome` directly. If the outcome reason starts with `__agent_error__:`, the orchestrator propagates it as `OrchestratorError::Agent` (infrastructure failure vs. domain failure).

### TaskStore

State management. Requires `Send`. Associated type: `type Task: TaskNode`.

**Queries:**

| Method | Returns | Purpose |
|---|---|---|
| `get(id)` | `Option<&Task>` | Read-only task access |
| `get_mut(id)` | `Option<&mut Task>` | Mutable task access |
| `task_count()` | `usize` | Total tasks (for limit checking) |
| `dfs_order(root)` | `Vec<TaskId>` | DFS traversal order from root |
| `save(path)` | `Result<()>` | Serialize store to disk |

**Lifecycle:**

| Method | Purpose |
|---|---|
| `set_root_id(id)` | Register root task ID |
| `bind_runtime()` | Re-inject non-serializable runtime deps after deserialization |

**Subtask creation:**

```rust
fn create_subtask(
    &mut self,
    parent_id: TaskId,
    spec: &SubtaskSpec,
    mark_fix: bool,
    inherit_recovery_rounds: Option<u32>,
) -> TaskId;
```

- `mark_fix`: true for fix-loop tasks, false for normal decomposition
- `inherit_recovery_rounds`: if `Some(n)`, child inherits parent's recovery counter (prevents exponential cost growth)
- Implementations panic if `parent_id` is not present in the store (store-invariant violation)

**Tree queries:**

| Method | Returns | Purpose |
|---|---|---|
| `any_non_fix_child_succeeded(parent_id)` | `bool` | At least one non-fix child succeeded? |
| `build_tree_context(id)` | `Result<TreeContext>` | Build owned snapshot of tree state around task |

---

## Protocol Types

### Identity and State Machine

**`TaskId`** — `u64` newtype. Display: `T{n}`. Copyable, hashable, serializable.

**`TaskPhase`** — State machine:

```
Pending → Assessing → Executing → Verifying → Completed
                                              ↗
Failed  ← (any state)
```

`TaskPhase::try_transition(new)` validates transitions. `Failed` is always valid from any state.

- Leaf path: `Pending → Assessing → Executing → Completed/Failed`
- Branch path: `Pending → Assessing → Executing → Verifying → Completed/Failed`

**`TaskPath`** — `Leaf` or `Branch`. Set by assessment.

**`Model`** — Ordered escalation hierarchy: `Haiku < Sonnet < Opus`. `Model::escalate()` returns `Option<Model>` (Opus → None).

### Assessment and Decomposition

| Type | Fields | Purpose |
|---|---|---|
| `AssessmentResult` | `path`, `model`, `rationale`, `magnitude: Option<Magnitude>` | Assessment decision |
| `Magnitude` | `max_lines_added`, `max_lines_modified`, `max_lines_deleted` | Size estimate (u64 fields) |
| `DecompositionResult` | `subtasks: Vec<SubtaskSpec>`, `rationale` | Decomposition output |
| `SubtaskSpec` | `goal`, `verification_criteria: Vec<String>`, `magnitude_estimate: MagnitudeEstimate` | Per-subtask specification |
| `MagnitudeEstimate` | `Small`, `Medium`, `Large` | Coarse size classification |

### Execution and Outcomes

| Type | Variants/Fields | Purpose |
|---|---|---|
| `TaskOutcome` | `Success`, `Failed { reason }` | Terminal task state |
| `LeafResult` | `outcome`, `discoveries: Vec<String>` | Leaf execution result |
| `BranchVerifyOutcome` | `Passed`, `Failed { reason }`, `FailedNoFixLoop { reason }` | Branch verification result |
| `VerificationOutcome` | `Pass`, `Fail { reason }` | Single verification step result |
| `VerificationResult` | `outcome`, `details` | Verification with explanation |

### Recovery and Fix Loop

| Type | Variants/Fields | Purpose |
|---|---|---|
| `FixBudgetCheck` | `WithinBudget { model }`, `Exhausted` | Fix loop budget decision |
| `ScopeCheck` | `WithinBounds`, `Exceeded { metric, actual, limit }` | Scope circuit breaker |
| `RecoveryEligibility` | `NotEligible { reason }`, `Eligible { round }` | Recovery availability |
| `RecoveryDecision` | `Unrecoverable { reason }`, `Plan { specs, supersede_pending }` | Recovery plan output |
| `RecoveryPlan` | `full_redecomposition`, `subtasks`, `rationale` | Recovery task specifications |

### Checkpoint and Context

| Type | Variants/Fields | Purpose |
|---|---|---|
| `CheckpointDecision` | `Proceed`, `Adjust { guidance }`, `Escalate` | Checkpoint classification |
| `ChildResponse` | `Continue`, `NeedRecoverySubtasks { specs, supersede_pending }`, `Failed(String)` | Parent's checkpoint response |
| `ResumePoint` | `Terminal(TaskOutcome)`, `LeafExecuting`, `LeafVerifying`, `BranchExecuting`, `BranchVerifying`, `NeedAssessment` | Resume routing decision |
| `SiblingSummary` | `id`, `goal`, `outcome`, `discoveries` | Completed sibling context |
| `ChildSummary` | `goal`, `status: ChildStatus`, `discoveries` | Child subtask context |
| `ChildStatus` | `Completed`, `Failed { reason }`, `Pending`, `InProgress` | Child state for context |

### Usage Tracking

**`SessionMeta`** — Per-agent-call metadata: `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `cost_usd`, `tool_calls`, `total_latency_ms`.

**`AgentResult<T>`** — Wraps a result value with `SessionMeta`.

**`TaskUsage`** — Cumulative per-task usage (all `SessionMeta` fields plus `api_calls`, `total_tool_calls`). `accumulate()` merges new session data. `zero()` default constructor.

---

## TreeContext

Read-only owned snapshot of tree state around a task. Built by `TaskStore::build_tree_context(id)` before every agent call.

```rust
pub struct TreeContext {
    pub parent_goal: Option<String>,
    pub parent_decomposition_rationale: Option<String>,
    pub parent_discoveries: Vec<String>,
    pub ancestor_goals: Vec<String>,
    pub completed_siblings: Vec<SiblingSummary>,
    pub pending_sibling_goals: Vec<String>,
    pub children: Vec<ChildSummary>,
    pub checkpoint_guidance: Option<String>,
}
```

Owned data (no references into the store) ensures agent code cannot invalidate references during async operations. Rebuilt before each `TaskNode` method call when task state may have changed.

---

## Orchestrator Coordination Loop

`Orchestrator<S: TaskStore>` is the main coordinator. Builder pattern: `Orchestrator::new(store, events).with_limits(config).with_state_path(path)`.

### Entry Point

`run(root_id)`:
1. Register root and pre-existing subtasks (emit `TaskRegistered` events in DFS order)
2. Call `execute_task(root_id)` recursively
3. Return root's `TaskOutcome`

### execute_task(id) — Core Recursion

Routes on `task.resume_point()`:

| ResumePoint | Action |
|---|---|
| `Terminal(outcome)` | Return outcome (already done) |
| `LeafExecuting` | Transition to Executing → `run_leaf` |
| `LeafVerifying` | `run_leaf` (skip assessment) |
| `BranchExecuting` | Transition to Executing → `execute_branch` → `finalize_branch` |
| `BranchVerifying` | `finalize_branch` (skip execution) |
| `NeedAssessment` | Full assessment flow (below) |

**Assessment flow** (NeedAssessment):
1. Transition: Pending → Assessing
2. Check `forced_assessment(max_depth)` — if Some, use it (root/depth forcing)
3. Otherwise call `task.assess(&ctx)`
4. Store via `set_assessment(path, model, magnitude)`
5. Emit `PathSelected`, `ModelSelected`
6. Transition: Assessing → Executing
7. Route: Leaf → `run_leaf`, Branch → `execute_branch` → `finalize_branch`

### run_leaf(id) — Leaf Execution

1. Build `TreeContext`
2. Call `task.execute_leaf(&ctx)` → `TaskOutcome`
3. If outcome reason starts with `__agent_error__:` → propagate as `OrchestratorError::Agent`
4. Complete or fail task, emit `TaskCompleted`, checkpoint

### execute_branch(id) — Branch Execution

**Decomposition phase:**
1. If `needs_decomposition()`: call `task.decompose(&ctx, model)` → `DecompositionResult`
2. Check task limit, create subtasks, emit `SubtasksCreated`

**Child iteration loop:**
```
loop {
    for each non-terminal child:
        outcome = execute_task(child_id)          // recursive
        if child has discoveries:
            parent.handle_checkpoint(&ctx, &discoveries)
            → Continue: proceed
            → NeedRecoverySubtasks: create recovery tasks, restart loop
            → Failed: return Failed
        if child failed:
            attempt_recovery(parent_id, reason)
            → recovery planned: restart loop
            → unrecoverable: return Failed
    if all children terminal: break
}
```

**Completion**: requires at least one non-fix child succeeded. Otherwise `Failed`.

### finalize_branch(id, outcome) — Branch Finalization

If outcome is Success:
1. Transition: Executing → Verifying
2. Call `task.verify_branch(&ctx)` → `BranchVerifyOutcome`
3. `Passed` → complete. `FailedNoFixLoop` → fail. `Failed` → enter fix loop.

### branch_fix_loop(id, failure_reason) — Fix Loop

```
loop {
    1. fix_round_budget_check(&limits)
       → Exhausted: fail task, return
       → WithinBudget { model }: continue
    2. increment_fix_rounds()
    3. check_branch_scope() — circuit breaker
       → Exceeded: fail with SCOPE_EXCEEDED
    4. Emit BranchFixRound
    5. task.design_fix(&ctx, reason, round, model)
       → Ok(Ok(specs)): create fix subtasks
       → Ok(Err(reason)): cannot fix, continue loop
       → Err(e): infrastructure error, propagate
    6. Execute fix children
    7. Re-verify: verify_branch(&ctx)
       → Passed: complete, return
       → Failed/FailedNoFixLoop: update reason, continue loop
}
```

### attempt_recovery(parent_id, failure_reason) — Recovery Planning

1. `can_attempt_recovery(&limits)` → `NotEligible`: return Failed
2. `assess_and_design_recovery(&ctx, reason, round)` → `RecoveryDecision`
3. `Unrecoverable`: return Failed
4. `Plan { specs, supersede_pending }`:
   - If `supersede_pending`: mark all Pending children as Failed
   - Create recovery subtasks (inherit parent's `recovery_rounds`)
   - Return None (child iteration loop restarts)

### Checkpoint Save

Called after phase transitions, task creation, and recovery decisions. Enables resume from crashes via `state_path`.

### Recursive Async

`execute_task` returns `Pin<Box<dyn Future + Send>>` — required for async recursion (Rust async fn cannot be directly recursive).

---

## Event System

Cue defines `CueEvent` (10 orchestration variants) and emits via the `EventEmitter<CueEvent>` trait from the `traits` crate. The orchestrator is generic over `T: EventEmitter<CueEvent>`. Application crates provide their own event enums and map via `From<CueEvent>`.

```rust
// traits crate
pub trait EventEmitter<E>: Send + Sync {
    fn emit(&self, event: E);
}

// cue/src/events.rs
pub enum CueEvent {
    TaskRegistered { .. },
    PhaseTransition { .. },
    PathSelected { .. },
    ModelSelected { .. },
    SubtasksCreated { .. },
    TaskCompleted { .. },
    TaskLimitReached { .. },
    BranchFixRound { .. },
    FixSubtasksCreated { .. },
    RecoverySubtasksCreated { .. },
}
```

### CueEvent Variants

**Lifecycle:** `TaskRegistered`, `PhaseTransition`, `PathSelected`, `ModelSelected`, `TaskCompleted`

**Decomposition:** `SubtasksCreated`, `FixSubtasksCreated`, `RecoverySubtasksCreated`

**Fix loop:** `BranchFixRound`

**Limits:** `TaskLimitReached`

Application-specific events (escalation, retry, checkpoint, vault, usage) are defined in the application crate (e.g., epic's `Event` enum with 24 variants).

---

## Configuration

### LimitsConfig

| Field | Default | Purpose |
|---|---|---|
| `max_depth` | 8 | Maximum task tree depth |
| `max_recovery_rounds` | 2 | Recovery attempts per task |
| `retry_budget` | 3 | Leaf retry attempts |
| `branch_fix_rounds` | 3 | Branch fix loop iterations |
| `root_fix_rounds` | 4 | Root fix loop iterations |
| `max_total_tasks` | 100 | Cumulative task creation limit |

Constructor clamps all values to minimum 1 (prevents zero-iteration loops). Immutable after orchestrator construction.

### VerificationStep

```rust
pub struct VerificationStep {
    pub name: String,
    pub command: Vec<String>,
    pub timeout: u32,           // Default: 300 seconds
}
```

---

## File Structure

```
cue/src/
  lib.rs              Public API re-exports
  traits.rs           TaskNode, TaskStore trait definitions
  types.rs            TaskId, TaskPhase, TaskPath, Model, Attempt, all protocol types
  context.rs          TreeContext
  events.rs           CueEvent enum (10 orchestration variants)
  config.rs           LimitsConfig, VerificationStep
  orchestrator.rs     Orchestrator<S: TaskStore, T: EventEmitter<CueEvent>>, OrchestratorError
```

Dependencies: `tokio` (sync), `serde`, `anyhow`, `thiserror`, `traits`. No AI/IO/network dependencies.
