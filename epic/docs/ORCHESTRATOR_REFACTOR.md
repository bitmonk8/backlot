# Orchestrator Refactor

## Problem

`orchestrator.rs` is 7,345 lines — 1,493 core, 5,851 test. The task module (`src/task/`) is 285 lines of pure data definitions with zero behavioral logic. All execution behavior lives in 27 methods on `Orchestrator<A>`.

The orchestrator is a god object. Tasks are empty shells.

## Principles

Derived from design discussion:

1. **Tasks own their behavior.** A task "does" things — execute, verify, fix. Functions implementing what a task does live with the task's data type.

2. **Tasks are self-contained.** A task has read access to the full tree (parent, siblings, children, ancestors). It has write access to itself only. It never executes another task, never mutates another task.

3. **The orchestrator is a pure coordinator.** It makes tasks do things. It never does task work itself. It owns the tree, manages task creation, drives execution order, and handles cross-task mutations.

4. **Decomposition is the orchestrator's reason to exist.** The orchestrator knowing about decomposition is natural — without it, there's no tree to orchestrate. But verification strategy, fix design, retry budgets, model escalation — those are task-internal.

## Architecture

### Data Flow

```
Orchestrator                          Task
    │                                   │
    │── build TreeContext ──────────────►│
    │── "do your thing" ────────────────►│
    │                                   │── reads tree (parent, siblings, ...)
    │                                   │── calls agent (via services)
    │                                   │── verifies own work
    │                                   │── fixes own work (retry, escalate)
    │                                   │── mutates only self
    │◄── result ────────────────────────│
    │                                   │
    │   (orchestrator interprets result,
    │    manages tree, recurses)
```

### Task Interfaces

#### Leaf

```rust
impl Task {
    /// Full leaf lifecycle: execute → verify → fix loop → return outcome.
    /// Handles retry/escalation (Haiku→Sonnet→Opus), scope circuit breaker,
    /// file-level review, verification gates — all internally.
    pub async fn execute_leaf<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> TaskOutcome;
}
```

One call. The orchestrator says "go", gets back success or terminal failure. Everything between — agent calls, verification, fix retries, model escalation, scope checking — is the leaf's business.

#### Branch

```rust
impl Task {
    /// Decompose into subtasks. Returns specs for the orchestrator to create.
    pub async fn decompose<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> Result<DecompositionResult>;

    /// Called after each child completes. Branch sees child via tree read access.
    /// Handles checkpoint classification, discovery propagation, recovery
    /// assessment and design — all internally.
    pub async fn on_child_completed<A: AgentService>(
        &mut self,
        child_id: TaskId,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> ChildResponse;

    /// Called after all children complete (or after fix subtasks complete).
    /// Branch verifies aggregate work. If verification fails, designs fix
    /// subtasks and requests their execution. Handles fix round budgets,
    /// model selection — all internally.
    pub async fn finalize_branch<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> BranchResult;
}

enum ChildResponse {
    /// Proceed to next child.
    Continue,
    /// Child failed; branch designed recovery subtasks.
    /// Orchestrator creates/executes them, then resumes child iteration.
    NeedRecoverySubtasks {
        specs: Vec<SubtaskSpec>,
        /// If true, pending siblings are superseded (full redecomposition).
        supersede_pending: bool,
    },
    /// Unrecoverable failure.
    Failed(String),
}

enum BranchResult {
    /// Branch verified successfully.
    Complete(TaskOutcome),
    /// Verification failed; branch designed fix subtasks.
    /// Orchestrator creates/executes them, then calls finalize_branch again.
    NeedSubtasks(Vec<SubtaskSpec>),
    /// Terminal failure (fix budget exhausted).
    Failed(String),
}
```

Three interaction points. The orchestrator never sees verification logic, fix design, checkpoint classification, recovery assessment, or model selection. It sees: "here are subtask specs", "continue", "I need these executed", or "I'm done."

### Orchestrator

The orchestrator shrinks to a coordinator loop:

```rust
impl<A: AgentService> Orchestrator<A> {
    pub async fn run(&mut self, root_id: TaskId) -> Result<()>;
    pub fn into_state(self) -> EpicState;

    /// Recursive dispatch. This is the only place child execution happens.
    async fn execute_task(&mut self, id: TaskId) -> Result<TaskOutcome> {
        // Assessment (with constraints: root=branch, max_depth=leaf)
        // ...
        match path {
            Leaf  => self.run_leaf(id).await,
            Branch => self.run_branch(id).await,
        }
    }

    async fn run_leaf(&mut self, id: TaskId) -> Result<TaskOutcome> {
        let tree = self.build_tree_context(id);
        let task = self.state.get_mut(id).unwrap();
        let outcome = task.execute_leaf(&tree, &self.services).await;
        self.complete_or_fail(id, outcome)
    }

    async fn run_branch(&mut self, id: TaskId) -> Result<TaskOutcome> {
        // 1. Decompose
        let tree = self.build_tree_context(id);
        let task = self.state.get_mut(id).unwrap();
        let result = task.decompose(&tree, &self.services).await?;
        let child_ids = self.create_subtasks(id, result.subtasks)?;

        // 2. Execute children
        'children: loop {
            for &child_id in self.state.get(id).unwrap().subtask_ids.iter() {
                if self.is_done(child_id) { continue; }
                self.execute_task(child_id).await?;

                let tree = self.build_tree_context(id);
                let task = self.state.get_mut(id).unwrap();
                match task.on_child_completed(child_id, &tree, &self.services).await {
                    ChildResponse::Continue => {}
                    ChildResponse::NeedRecoverySubtasks { specs, supersede_pending } => {
                        if supersede_pending {
                            self.fail_pending_children(id);
                        }
                        self.create_subtasks(id, specs)?;
                        continue 'children;
                    }
                    ChildResponse::Failed(reason) => {
                        return self.complete_or_fail(id, TaskOutcome::Failed { reason });
                    }
                }
            }
            break;
        }

        // 3. Finalize (verify, possibly fix)
        loop {
            let tree = self.build_tree_context(id);
            let task = self.state.get_mut(id).unwrap();
            match task.finalize_branch(&tree, &self.services).await {
                BranchResult::Complete(outcome) => {
                    return self.complete_or_fail(id, outcome);
                }
                BranchResult::NeedSubtasks(specs) => {
                    let fix_ids = self.create_subtasks(id, specs)?;
                    for &fix_id in &fix_ids {
                        self.execute_task(fix_id).await?;
                    }
                    // Loop back to finalize_branch
                }
                BranchResult::Failed(reason) => {
                    return self.complete_or_fail(id, TaskOutcome::Failed { reason });
                }
            }
        }
    }

    // Cross-task operations (the only mutations the orchestrator performs)
    fn create_subtasks(&mut self, parent_id: TaskId, specs: Vec<SubtaskSpec>) -> Result<Vec<TaskId>>;
    fn fail_pending_children(&mut self, parent_id: TaskId);
    fn complete_or_fail(&mut self, id: TaskId, outcome: TaskOutcome) -> Result<TaskOutcome>;
    fn build_tree_context(&self, id: TaskId) -> TreeContext;
}
```

The orchestrator contains zero task-type-specific logic. It knows three things:
1. Leaves execute in one shot.
2. Branches decompose, then children execute, then the branch finalizes.
3. Tasks may request subtasks be created and executed.

### TreeContext

Read-only snapshot of tree state, built by the orchestrator before calling a task method. The task uses this + `&self` to understand its position and build agent prompts.

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

Built from `&EpicState` before the `get_mut(id)` borrow. This avoids the `&state` / `&mut task` borrow conflict — `TreeContext` is owned data, not a reference into state.

The task assembles `TaskContext` (for agent calls) from `&self` + `&TreeContext`:

```rust
impl Task {
    fn build_agent_context(&self, tree: &TreeContext) -> TaskContext { ... }
}
```

### Services

Shared infrastructure, immutable for the duration of a run:

```rust
pub struct Services<A: AgentService> {
    pub agent: A,
    pub events: EventSender,
    pub vault: Option<Arc<vault::Vault>>,
    pub limits: LimitsConfig,
    pub project_root: Option<PathBuf>,
}
```

Tasks call `svc.agent.*()` for LLM interactions, `svc.events.send()` for events, `svc.vault` for knowledge recording. Tasks emit their own events (FixAttempt, ModelEscalated, VerificationResult, etc.).

### Task Self-Contained Mutations

The `Task` struct gains behavior methods for mutations currently scattered across the orchestrator. 11 of 14 current mutation sites are self-contained (only touch the task being operated on):

```rust
impl Task {
    pub fn set_assessment(&mut self, path: TaskPath, model: Model, magnitude: Option<Magnitude>);
    pub fn record_attempt(&mut self, attempt: Attempt, is_fix: bool);
    pub fn record_discoveries(&mut self, discoveries: Vec<String>);
    pub fn set_model(&mut self, model: Model);
    pub fn set_decomposition_rationale(&mut self, rationale: String);
    pub fn set_checkpoint_guidance(&mut self, guidance: Option<String>);
    pub fn append_checkpoint_guidance(&mut self, new_guidance: &str);
    pub fn increment_fix_rounds(&mut self) -> u32;
    pub fn increment_recovery_rounds(&mut self) -> u32;
    pub fn accumulate_usage(&mut self, meta: &SessionMeta);
    pub fn trailing_attempts_at_tier(&self, model: Model, is_fix: bool) -> u32;
}
```

### What Lives Where

| Concern | Location | Justification |
|---------|----------|---------------|
| Leaf execution + retry/escalation | `task/leaf.rs` | Task behavior — the leaf knows how to do its job |
| Leaf verification + fix loop | `task/leaf.rs` | Task self-checks its own work |
| Leaf scope circuit breaker | `task/leaf.rs` (calls `scope.rs`) | Task decides if it's exceeding bounds |
| Branch decomposition | `task/branch.rs` | Task behavior — the branch knows how to break down work |
| Branch checkpoint/discovery | `task/branch.rs` | Task evaluates child outcomes |
| Branch recovery assessment + design | `task/branch.rs` | Task decides how to recover from child failure |
| Branch verification + fix design | `task/branch.rs` | Task self-checks aggregate work |
| Agent context assembly | `task/mod.rs` | Task knows what context the agent needs |
| Scope checking utilities | `task/scope.rs` | Shared utility, used by leaf and branch |
| Tree context assembly | `orchestrator/context.rs` | Reads full state — orchestrator-level |
| Recursive child execution | `orchestrator/mod.rs` | Cross-task — only the coordinator recurses |
| Subtask creation (ID gen, insert) | `orchestrator/mod.rs` | Cross-task — tree mutation |
| Sibling failure marking | `orchestrator/mod.rs` | Cross-task — multi-task mutation |
| State persistence | `orchestrator/mod.rs` | Global concern |
| Event emission (tree-level) | `orchestrator/mod.rs` | TaskRegistered, SubtasksCreated, TaskCompleted |
| Event emission (task-level) | `task/*.rs` | FixAttempt, ModelEscalated, DiscoveriesRecorded, etc. |

## File Structure

```
src/
  task/
    mod.rs          Task struct, types, self-contained mutation methods,
                    assessment, agent context assembly
    leaf.rs         execute_leaf(): full lifecycle (execute → verify → fix loop)
                    Retry/escalation state machine, scope checking, file-level review
    branch.rs       decompose(), on_child_completed(), finalize_branch()
                    Checkpoint, recovery, verification, fix design
    scope.rs        Scope circuit breaker, git_diff_numstat, evaluate_scope
    assess.rs       AssessmentResult (existing, unchanged)
    verify.rs       VerificationOutcome, VerificationResult (existing, unchanged)
  orchestrator/
    mod.rs          Orchestrator struct, run(), execute_task(), run_leaf(),
                    run_branch(), create_subtasks(), fail_pending_children()
    context.rs      build_tree_context() — reads EpicState, returns TreeContext
    services.rs     Services struct
  state.rs          EpicState (unchanged)
  events.rs         Event types (unchanged)
  agent/            AgentService trait, ReelAgent (unchanged)
  ...
```

### Estimated Core Line Counts

| File | Est. lines | Change |
|------|-----------|--------|
| `task/mod.rs` | ~350 | was 228; gains mutation methods + context assembly |
| `task/leaf.rs` | ~400 | was 2; gains full leaf lifecycle from orchestrator |
| `task/branch.rs` | ~400 | was 24; gains decompose, checkpoint, recovery, verify, fix |
| `task/scope.rs` | ~100 | new; from orchestrator free functions |
| `orchestrator/mod.rs` | ~200 | was 1,493; now pure coordination |
| `orchestrator/context.rs` | ~120 | from orchestrator's build_context |
| `orchestrator/services.rs` | ~60 | new |
| **Total core** | ~1,630 | was 1,778 (orchestrator 1,493 + task 285) |

Task module grows from 285 → ~1,250 lines. Orchestrator shrinks from 1,493 → ~380 lines. The weight shifts to where the behavior belongs.

### Test Distribution

Tests follow behavior:

| File | Tests | Content |
|------|-------|---------|
| `task/leaf.rs` | ~14 | execution, retry/escalation, fix loop, fix resume |
| `task/branch.rs` | ~19 | decomposition, checkpoint (9), branch fix (7), recovery (partially) |
| `task/scope.rs` | ~5 | scope evaluation pure functions |
| `task/mod.rs` | grows | tests for mutation methods |
| `orchestrator/mod.rs` | ~7 | resume/dispatch, child loop, cross-task operations |
| `orchestrator/context.rs` | ~2 | context assembly |

The bulk of the 5,851 test lines moves into `task/` — where the behavior now lives.

## Borrow Pattern

The `&state` / `&mut task` conflict is resolved by building `TreeContext` (owned data) before borrowing `&mut Task`:

```rust
// Step 1: Read state, produce owned snapshot
let tree = self.build_tree_context(id);  // borrows &self.state, returns owned TreeContext

// Step 2: Get mutable task (no conflict — tree is owned, not a reference)
let task = self.state.get_mut(id).unwrap();

// Step 3: Task uses owned tree + &mut self
let outcome = task.execute_leaf(&tree, &self.services).await;
```

The task sees a frozen snapshot of the tree. During leaf execution (sequential, no parallel tasks), nothing else modifies the tree, so the snapshot stays consistent.

For branch `on_child_completed`, the orchestrator rebuilds `TreeContext` after each child completes — the child's outcome is now visible in the fresh snapshot.

## State Persistence

The orchestrator saves state after:
- Task creation (subtasks added to tree)
- Task completion/failure (phase transition)
- Between child executions (crash recovery resumes from last completed child)

Tasks do NOT trigger saves during their internal execution. If the process crashes mid-leaf-execution, the task restarts from its last persisted phase. The task examines its own `attempts` / `fix_attempts` to understand prior work (existing resume behavior).

## Migration Strategy

Each step compiles and tests pass.

1. `orchestrator.rs` → `orchestrator/mod.rs`
2. Extract `Services` → `orchestrator/services.rs`
3. Extract `TreeContext` builder → `orchestrator/context.rs`
4. Add `Task` mutation methods in `task/mod.rs`, update orchestrator to call them
5. Extract scope → `task/scope.rs`
6. Move leaf execution logic → `task/leaf.rs` (+ tests)
7. Move branch logic → `task/branch.rs` (+ tests)
8. Introduce `ChildResponse` / `BranchResult` enums, refactor orchestrator to use them
9. Slim `orchestrator/mod.rs` to pure coordination
10. Verify no task-type-specific logic remains in orchestrator
