# Orchestrator Extraction Spec

## Goal

Extract the orchestrator (recursive problem-solver coordination engine) from epic into a standalone sibling crate. The orchestrator becomes a generic framework for recursive task decomposition with retry, escalation, fix loops, and recovery. Epic provides the concrete task implementation (AI agent calls, prompts, wire formats) through traits defined by the orchestrator crate.

**Key constraint**: Epic's `Task` struct, its lifecycle methods (`execute_leaf`, `verify_branch`, etc.), and all AI-specific logic stay in epic. The orchestrator crate defines the coordination algorithm and the trait contract that tasks must satisfy.

Proposed crate name: **score** (Structured Coordinator for Orchestrated Recursive Execution). Open to alternatives.

---

## Architecture

```
score (orchestrator crate)
  - Defines: TaskNode trait, coordination loop, state machine, events, tree context
  - Generic over: T: TaskNode

epic (application crate)
  - Defines: Task struct (implements score::TaskNode)
  - Defines: ReelAgent (implements score::AgentService)
  - Owns: CLI, TUI, prompts, wire formats, knowledge/research, config shell
  - Depends on: score, reel, vault, lot
```

The orchestrator never constructs tasks directly — it calls trait methods to create them. It never reads task fields directly — it calls trait accessors. It never performs AI operations — it calls trait lifecycle methods that epic implements.

---

## Trait Design

The orchestrator's coupling to Task is deep: ~15 field reads, ~12 mutation methods, ~8 async lifecycle methods, plus task construction. This must be captured in traits.

### Approach: Two-Trait Split

**`TaskNode`** — Data access, decisions, mutations, and lifecycle. Implemented by epic's `Task`.

**`AgentService`** — Already exists. The orchestrator's interface to AI models. Stays as-is (already a trait). Implemented by epic's `ReelAgent`.

The lifecycle methods currently on Task (`execute_leaf`, `verify_branch`, etc.) call `AgentService` internally. They stay in epic as `TaskNode` method implementations.

### Decision Collapsing

Before defining the trait, push coordinator decisions into task methods. The coordinator currently reads multiple task fields to make decisions that the task should own. Each collapsed decision removes field accessors from the trait surface.

| # | Coordinator pattern | Fields read | Collapsed method | Return type |
|---|---|---|---|---|
| 1 | Resume: match `(path, phase)` to determine re-entry point | `path`, `phase` | `resume_point()` | `ResumePoint { LeafExecuting, LeafVerifying, BranchExecuting, BranchVerifying, Terminal(TaskOutcome), NeedAssessment }` |
| 2 | Assessment: root forces branch, depth-cap forces leaf | `parent_id`, `depth` | `forced_assessment(max_depth)` | `Option<AssessmentResult>` |
| 3 | After branch verify: fix tasks skip fix loop | `is_fix_task` | Fold into `verify_branch` return | `BranchVerifyOutcome::FailedNoFixLoop` variant, or separate `should_enter_fix_loop()` |
| 4 | Fix budget: passes `is_root` computed from `parent_id` | `parent_id` | Remove `is_root` param from `fix_round_budget_check` | Task knows its own parent_id |
| 5 | Decomposition: check if already decomposed + select model | `subtask_ids`, `current_model` | `needs_decomposition()`, `decompose_model()` | `bool`, `Model` |
| 6 | Recovery: fix tasks can't recover + budget check + round calc | `is_fix_task`, `recovery_rounds` | `can_attempt_recovery(limits)` | `RecoveryEligibility { NotEligible(reason), Eligible { round } }` |
| 7 | All-non-fix-children-failed guard (cross-task) | per-child: `is_fix_task`, `phase` | Move to `TaskStore::any_non_fix_child_succeeded(id)` | `bool` |
| 8 | TUI registration: emit event with task metadata | `parent_id`, `goal`, `depth`, `phase` | `registration_info()` | `RegistrationInfo { parent_id, goal, depth, phase }` |
| 9 | Terminal check: skip completed/failed tasks | `phase` | `is_terminal()` | `bool` |

### TaskNode Trait Surface (After Collapsing)

```rust
// --- Read accessors (minimal: only fields the coordinator cannot avoid reading) ---

fn id(&self) -> TaskId;
fn parent_id(&self) -> Option<TaskId>;   // needed for create_subtasks depth calc
fn goal(&self) -> &str;                   // needed for registration events
fn depth(&self) -> u32;                   // needed for create_subtasks depth calc
fn phase(&self) -> TaskPhase;             // needed for phase transitions, child loop
fn subtask_ids(&self) -> &[TaskId];       // needed for child iteration loop
fn discoveries(&self) -> &[String];       // needed for checkpoint trigger
fn recovery_rounds(&self) -> u32;         // needed for child inheritance

// --- Decision methods (replace multi-field reads) ---

fn is_terminal(&self) -> bool;
fn resume_point(&self) -> ResumePoint;
fn forced_assessment(&self, max_depth: u32) -> Option<AssessmentResult>;
fn needs_decomposition(&self) -> bool;
fn decompose_model(&self) -> Model;
fn registration_info(&self) -> RegistrationInfo;

// --- Mutations ---

fn set_phase(&mut self, phase: TaskPhase);
fn set_assessment(&mut self, path: TaskPath, model: Model, magnitude: Option<Magnitude>);
fn set_decomposition_rationale(&mut self, rationale: String);
fn set_subtask_ids(&mut self, ids: &[TaskId], append: bool);
fn increment_fix_rounds(&mut self) -> u32;
fn accumulate_usage(&mut self, meta: &SessionMeta) -> f64;  // returns new total cost

// --- Lifecycle (async) ---

async fn execute_leaf(&mut self, ctx: &TreeContext, svc: &Services<A>)
    -> TaskOutcome;

async fn verify_branch(&mut self, ctx: &TreeContext, svc: &Services<A>)
    -> BranchVerifyOutcome;   // includes FailedNoFixLoop for fix tasks

async fn fix_round_budget_check(&self, limits: &LimitsConfig)
    -> FixBudgetCheck;        // no is_root param; task knows internally

async fn check_branch_scope(&self, svc: &Services<A>)
    -> ScopeCheck;

async fn design_fix(&mut self, ctx: &TreeContext, svc: &Services<A>,
    failure_reason: &str, round: u32, model: Model)
    -> Result<DecompositionResult>;

async fn handle_checkpoint(&mut self, ctx: &TreeContext, svc: &Services<A>,
    discoveries: &[String])
    -> ChildResponse;

fn can_attempt_recovery(&self, limits: &LimitsConfig)
    -> RecoveryEligibility;   // collapses is_fix_task + budget + round

async fn assess_and_design_recovery(&mut self, ctx: &TreeContext, svc: &Services<A>,
    failure_reason: &str, round: u32)
    -> RecoveryDecision;

fn verification_model(&self) -> Model;
```

**Trait surface**: 8 read accessors, 6 decision methods, 6 mutations, 9 lifecycle methods = **29 methods total**.

Without collapsing: 16 read accessors, 9 mutations, 9 lifecycle methods = **34 methods total**, and the coordinator contains decision logic that belongs to the task.

The collapsed version is smaller AND cleaner — the coordinator becomes a pure state-machine driver that asks the task what to do, rather than inspecting task internals to decide.

### TaskStore Trait

The orchestrator creates, stores, looks up, and queries across tasks. Currently done via `EpicState`. Abstract this:

```rust
trait TaskStore {
    type Task: TaskNode;

    fn get(&self, id: TaskId) -> Option<&Self::Task>;
    fn get_mut(&mut self, id: TaskId) -> Option<&mut Self::Task>;
    fn task_count(&self) -> usize;
    fn dfs_order(&self, root: TaskId) -> Vec<TaskId>;
    fn set_root_id(&mut self, id: TaskId);
    fn save(&self, path: &Path) -> Result<()>;

    // --- Task creation (replaces Task::new + field mutation in coordinator) ---
    fn create_subtask(&mut self, parent_id: TaskId, spec: &SubtaskSpec,
        mark_fix: bool, inherit_recovery_rounds: Option<u32>) -> TaskId;

    // --- Cross-task queries (moved from coordinator) ---
    fn any_non_fix_child_succeeded(&self, parent_id: TaskId) -> bool;

    // --- Tree context building (moved from orchestrator/context.rs) ---
    fn build_tree_context(&self, id: TaskId) -> Result<TreeContext, OrchestratorError>;
    fn build_task_context(&self, id: TaskId) -> Result<TaskContext, OrchestratorError>;
}
```

Epic's `EpicState` implements `TaskStore` with `type Task = Task`.

Key changes from the current design:
- **`create_subtask`** replaces the coordinator's `Task::new()` + field mutation sequence. The store implementation (in epic) constructs the concrete Task from the SubtaskSpec. Coordinator no longer needs to know Task's constructor signature or mutable fields.
- **`any_non_fix_child_succeeded`** absorbs the cross-task guard that currently reads `is_fix_task` + `phase` on each child. Removes `is_fix_task` from the TaskNode accessor surface.
- **`build_tree_context` / `build_task_context`** move from `orchestrator/context.rs` into TaskStore. These are tree-level queries that access Task fields through the store's internal `HashMap`, not through the trait. This eliminates the need for TreeContext building to use TaskNode accessors (it uses concrete Task fields internally).

### Services Becomes Non-Generic Over Task

Currently `Services<A: AgentService>`. With the task trait approach, Services needs to carry things the lifecycle methods need — but those are epic's concern, not the orchestrator's. The orchestrator passes Services through to TaskNode methods opaquely.

**Option A: Services carries the AgentService + infrastructure**

```rust
// In orchestrator crate
pub struct Services<A: AgentService> {
    pub agent: A,
    pub events: EventSender,
    pub vault: Option<Arc<vault::Vault>>,
    pub limits: LimitsConfig,
    pub project_root: Option<PathBuf>,
    pub state_path: Option<PathBuf>,
}
```

TaskNode lifecycle methods receive `&Services<A>`. Epic's Task implementation accesses `svc.agent` to make AI calls. This keeps the current pattern — Services is the orchestrator's injection point.

**Option B: Services is opaque to the orchestrator**

The orchestrator defines Services as a generic parameter too: `Orchestrator<T: TaskNode, S: ServiceBundle>`. This is more abstract but adds complexity for no clear benefit right now.

**Recommendation**: Option A. Services stays concrete, parameterized by `AgentService`. The orchestrator crate defines Services and passes it through. Epic's TaskNode impls use it.

### What the Orchestrator Crate Defines

| Category | Types |
|---|---|
| Core identity | `TaskId` |
| State machine | `TaskPhase`, `TaskPath`, `Model` |
| Decision enums | `BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `ChildResponse`, `CheckpointDecision`, `ScopeCheck` |
| Results | `TaskOutcome`, `LeafResult`, `RecoveryPlan`, `DecompositionResult`, `SubtaskSpec`, `AssessmentResult`, `VerificationOutcome`, `VerificationResult`, `VerifyOutcome` |
| Data | `Attempt`, `Magnitude`, `MagnitudeEstimate`, `TaskUsage`, `SessionMeta`, `AgentResult<T>` |
| Context | `TaskContext`, `TreeContext`, `SiblingSummary`, `ChildStatus`, `ChildSummary` |
| Infrastructure | `Services<A>`, `EventSender`, `EventReceiver`, `Event`, `event_channel()` |
| Config | `LimitsConfig`, `VerificationStep` |
| Traits | `TaskNode`, `TaskStore`, `AgentService` |
| Coordinator | `Orchestrator<T, S, A>`, `OrchestratorError` |

### What Epic Defines

| Category | Content |
|---|---|
| Task impl | `Task` struct, `impl TaskNode for Task`, all lifecycle method bodies |
| State impl | `EpicState`, `impl TaskStore for EpicState` |
| Agent impl | `ReelAgent`, `impl AgentService for ReelAgent` |
| Prompts | All prompt assembly (`prompts.rs`) |
| Wire formats | All LLM structured output types (`wire.rs`) |
| Knowledge | `ResearchTool`, gap-filling pipeline |
| Config | `EpicConfig`, `ModelConfig`, `VaultConfig`, `ProjectConfig` |
| CLI/TUI | Everything presentation-layer |

---

## Scope of Extraction (Revised)

### What Moves to the New Crate

| Current location | Content |
|---|---|
| `orchestrator/mod.rs` | Coordinator logic (now generic over `TaskNode + TaskStore`) |
| `orchestrator/context.rs` | `TreeContext` struct + `build_tree_context()` (now calls `TaskNode` accessors) |
| `orchestrator/services.rs` | `Services<A>` |
| `agent/mod.rs` (partial) | `AgentService` trait, `TaskContext`, `SessionMeta`, `AgentResult<T>`, `SiblingSummary`, `ChildStatus`, `ChildSummary` |
| `events.rs` | `Event`, `EventSender`, `EventReceiver`, `event_channel()` |
| `config/project.rs` (partial) | `LimitsConfig`, `VerificationStep` |

Plus new trait definitions: `TaskNode`, `TaskStore`.

Plus types currently in task/ that are part of the orchestration protocol (not task-specific): `TaskId`, `TaskPhase`, `TaskPath`, `Model`, `TaskOutcome`, `Attempt`, `Magnitude`, `MagnitudeEstimate`, `TaskUsage`, `LeafResult`, `RecoveryPlan`, `AssessmentResult`, `SubtaskSpec`, `DecompositionResult`, `CheckpointDecision`, `BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `ChildResponse`, `ScopeCheck`, `VerificationOutcome`, `VerificationResult`, `VerifyOutcome`.

### What Stays in Epic

| Current location | Content | Why |
|---|---|---|
| `task/mod.rs` | `Task` struct, `Task::new()`, mutation method bodies | Concrete task implementation |
| `task/leaf.rs` | `Task::execute_leaf()` and helpers | AI-specific lifecycle logic |
| `task/branch.rs` | Branch `Task` methods | AI-specific branch decisions |
| `task/scope.rs` | `git_diff_numstat`, `evaluate_scope` | Could go either way; see below |
| `state.rs` | `EpicState` struct + persistence | Concrete state implementation |
| `main.rs` | CLI entry point | Application shell |
| `cli.rs`, `init.rs` | CLI commands | Application shell |
| `tui/` | TUI rendering | Presentation layer |
| `sandbox.rs` | Container detection | Startup check |
| `agent/reel_adapter.rs` | `ReelAgent` | Concrete agent |
| `agent/prompts.rs` | Prompt templates | Epic-specific |
| `agent/wire.rs` | Wire format types | Epic-specific |
| `knowledge.rs` | Research service | Epic-specific |
| `config/project.rs` (partial) | `EpicConfig`, `ModelConfig`, `VaultConfig` | Epic-specific config |
| `test_support.rs` | `MockAgentService`, `MockBuilder` | See challenges section |

---

## Dependency Analysis

### Vault Integration

The orchestrator currently calls `vault::Vault::record()` and `vault::Vault::reorganize()` directly in `orchestrator/mod.rs`.

With Task staying in epic, most vault calls already route through Task lifecycle methods (leaf discovers → records to vault). The orchestrator's direct vault calls are:
- `record()` for verification failures and checkpoint decisions (in coordinator code)
- `reorganize()` after root branch children complete

**Options**:
- **(A) Accept vault dependency in orchestrator crate.** Vault remains in Services, orchestrator calls it directly for the coordinator-level operations.
- **(B) Abstract vault behind a trait.** `DocumentStore` trait with `record()` and `reorganize()`. Orchestrator crate defines the trait, epic provides vault-backed impl.
- **(C) Push vault calls into TaskNode.** Add methods like `record_discovery()`, `on_branch_children_complete()` to TaskNode. Task implementation handles vault internally.

**Recommendation**: **(B)** now makes more sense given the generic approach. If we're already defining TaskNode and TaskStore traits, adding a small DocumentStore trait is consistent. Keeps the orchestrator crate free of vault/reel dependencies entirely.

### Scope Circuit Breaker

`task/scope.rs` contains `git_diff_numstat()` (shells out to git) and `evaluate_scope()` (pure comparison). Currently called by Task lifecycle methods.

**Options**:
- Move to orchestrator crate (it's a reusable coordination mechanism)
- Keep in epic (it's called from Task methods which stay in epic)

**Recommendation**: Keep in epic. The Task lifecycle methods call it, and those stay in epic. The orchestrator doesn't call scope functions directly — it calls `task.check_branch_scope()` which is a TaskNode method.

### TreeContext Building

`build_tree_context()` reads task fields to construct context. Currently uses direct field access on `Task`. After extraction, it calls `TaskNode` trait accessors.

This function moves to the orchestrator crate. It becomes generic: `build_tree_context<S: TaskStore>(store: &S, id: TaskId) -> TreeContext`.

The `to_task_context()` method on TreeContext (which builds `TaskContext` from TreeContext + Task) also moves — it calls TaskNode accessors.

---

## Preparatory Refactoring

Work happens in epic before extraction. Each phase builds on the previous. Tests must pass after every step.

### Phase 1: Push decisions into Task methods

Refactor the coordinator to stop inspecting task internals for decisions the task should own. No trait definitions yet — this is pure refactoring of existing concrete code.

#### 1a. `Task::is_terminal() -> bool`

Replace `match task.phase { Completed | Failed => ... }` in coordinator with `task.is_terminal()`. Used in `execute_task` resume (line 405) and child iteration loop (line 732).

#### 1b. `Task::resume_point() -> ResumePoint`

Replace the two-block resume logic (lines 403-442) that reads `path` + `phase` with a single method returning an enum. Define `ResumePoint` in the task module.

```rust
enum ResumePoint {
    Terminal(TaskOutcome),
    LeafExecuting,
    LeafVerifying,
    BranchExecuting,
    BranchVerifying,
    NeedAssessment,
}
```

#### 1c. `Task::forced_assessment(max_depth) -> Option<AssessmentResult>`

Replace the root/depth-cap logic (lines 446-468) with a method that returns `Some(forced_result)` or `None` (needs real assessment). Eliminates coordinator reading `parent_id` and `depth` for this purpose.

#### 1d. Remove `is_root` param from `fix_round_budget_check`

Currently coordinator computes `is_root = task.parent_id.is_none()` and passes it in. Task already has `parent_id` — it can compute this internally.

#### 1e. `Task::needs_decomposition() -> bool` and `Task::decompose_model() -> Model`

Replace `subtask_ids.is_empty()` and `current_model.unwrap_or(Sonnet)` reads (lines 681-689).

#### 1f. `Task::can_attempt_recovery(limits) -> RecoveryEligibility`

Collapse the three-step check (lines 875-894): `is_fix_task` guard, `recovery_budget_check`, round calculation. Return enum:

```rust
enum RecoveryEligibility {
    NotEligible { reason: String },
    Eligible { round: u32 },
}
```

#### 1g. `Task::registration_info() -> RegistrationInfo`

Bundle the 4 fields read for TUI registration events (lines 149-162, 372-379).

#### 1h. Handle `is_fix_task` in `verify_branch` return

Currently coordinator reads `is_fix_task` after `verify_branch` to decide whether to enter fix loop (line 530). Either add a `FailedNoFixLoop` variant to `BranchVerifyOutcome`, or add `Task::should_enter_fix_loop() -> bool`.

### Phase 2: Move cross-task queries to EpicState

#### 2a. `EpicState::any_non_fix_child_succeeded(parent_id) -> bool`

Move the all-non-fix-children-failed guard (lines 845-854) from the coordinator into EpicState. Eliminates coordinator reading `is_fix_task` + `phase` on individual children.

#### 2b. Move `build_tree_context` into EpicState

`build_tree_context` reads ~10 task fields to construct TreeContext. Currently a free function in `orchestrator/context.rs`. Move it to `EpicState::build_tree_context()`. The function accesses tasks through the internal HashMap, not through external accessors. This eliminates a large surface area from the trait boundary.

Similarly move `build_context` (which combines TreeContext + task clone into TaskContext).

#### 2c. Move `create_subtasks` task-construction logic into EpicState

Coordinator's `create_subtasks` (lines 327-384) calls `Task::new()` + sets fields + inserts. Move the Task-constructing portion to `EpicState::create_subtask(parent_id, spec, mark_fix, inherit_recovery_rounds) -> TaskId`. Coordinator keeps the event emission and task-limit checking. Eliminates coordinator knowing Task's constructor.

### Phase 3: Decouple types into separable files

No behavioral changes — just file organization to make the extraction cut mechanical.

#### 3a. Extract `AgentService` + context types from `agent/mod.rs`

Move `AgentService`, `TaskContext`, `SessionMeta`, `AgentResult<T>`, `SiblingSummary`, `ChildStatus`, `ChildSummary` to `src/agent/traits.rs`. `agent/mod.rs` re-exports.

#### 3b. Extract orchestration-protocol types from `task/mod.rs`

Move `TaskId`, `TaskPhase`, `TaskPath`, `Model`, `Attempt`, `Magnitude`, `MagnitudeEstimate`, `TaskUsage`, `TaskOutcome`, `LeafResult`, `RecoveryPlan` to `src/task/types.rs`. `task/mod.rs` re-exports.

#### 3c. Extract orchestration-protocol types from `task/branch.rs`

Move `SubtaskSpec`, `DecompositionResult`, `CheckpointDecision`, `BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `ChildResponse` to their own file. `branch.rs` re-exports.

#### 3d. Split `LimitsConfig` / `VerificationStep` from config

Move to `src/config/limits.rs`. `EpicConfig` imports them.

#### 3e. Abstract vault behind `DocumentStore` trait

Define trait with `record()` and `reorganize()` methods. Implement for `vault::Vault`. Change Services to hold `Option<Arc<dyn DocumentStore>>`.

#### 3f. Deduplicate verify/scope helpers

Consolidate the duplicated `record_to_vault`, `emit_usage_event`, `try_verify`, `try_file_level_review`, `verification_model`, `check_scope` methods.

### Phase 4: Define traits

With decisions pushed in and types separated, define `TaskNode` and `TaskStore` traits. Implement for `Task` and `EpicState`. Make `Orchestrator` generic.

This is the final prep step. At this point the extraction boundary is clean and the mechanical extraction (Phase 5) is trivial.

### Phase 5: Verify clean boundary

Audit that the code destined for the orchestrator crate has no imports from: `tui`, `cli`, `init`, `sandbox`, `knowledge`, `reel_adapter`, `prompts`, `wire`, `vault` (now behind trait).

### Phase 6: Mechanical extraction

Move files to new crate, update import paths, publish, add git dependency.

---

## New Crate Structure

```
score/
├── Cargo.toml                # workspace root
├── score/                     # library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # Public API re-exports
│       ├── traits.rs          # TaskNode, TaskStore, AgentService, DocumentStore
│       ├── types.rs           # TaskId, TaskPhase, TaskPath, Model, Attempt, etc.
│       ├── context.rs         # TaskContext, TreeContext, SiblingSummary, etc.
│       ├── events.rs          # Event, EventSender, EventReceiver
│       ├── config.rs          # LimitsConfig, VerificationStep
│       ├── orchestrator/
│       │   ├── mod.rs         # Coordinator (generic over TaskNode + TaskStore)
│       │   └── services.rs    # Services<A>
│       └── outcomes.rs        # BranchVerifyOutcome, FixBudgetCheck, RecoveryDecision, etc.
```

### Dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["sync"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
```

No vault, reel, flick, or lot dependency. The orchestrator is a pure coordination framework.

---

## Challenges and Risks

### 1. Test infrastructure split

`MockAgentService` (648 lines) implements `AgentService` — this trait moves to the orchestrator crate. The mock must either:
- Move to the orchestrator crate (behind `#[cfg(test)]` or a `test-support` feature)
- Stay in epic and implement the trait from the orchestrator crate
- Be split: orchestrator crate gets a minimal mock, epic keeps the full MockBuilder

**Recommendation**: Orchestrator crate gets a minimal mock or test-support feature. Epic's `MockBuilder` wraps or re-exports it with epic-specific conveniences.

### 2. Async trait methods

`TaskNode` has async lifecycle methods. Rust 2024 edition supports `async fn` in traits with `impl Future` return types (RPITIT). This works if the trait is not object-safe. If object safety is needed later, `async-trait` or manual boxing would be required.

The orchestrator is currently generic (`Orchestrator<A: AgentService>`) not dynamic. Same pattern works: `Orchestrator<T: TaskNode, S: TaskStore<Task = T>, A: AgentService>`. Three type parameters is verbose but functional.

### 3. State file compatibility

`EpicState` serialization includes `Task` (with all its fields). The `Task` struct stays in epic. The orchestrator-protocol types (`TaskId`, `TaskPhase`, etc.) move to the orchestrator crate but their serde representation is unchanged. Backward compatibility maintained.

### 4. Task construction

Addressed by prep Phase 2c: `EpicState::create_subtask()` absorbs the `Task::new()` + field mutation + insert sequence. Coordinator just gets back a `TaskId`.

---

## Open Questions

1. **Crate name**: "score", "conductor", "grove", "arbor", or "epic-core"?
2. **DocumentStore trait**: Worth the abstraction (zero sibling-crate deps), or accept vault dependency (simpler)?
3. **Type parameter ergonomics**: `Orchestrator<T, S, A>` has three generic params. Could use associated types on `TaskStore` (`type Task: TaskNode`, `type Agent: AgentService`) to reduce to `Orchestrator<S: TaskStore>`.
4. **`TaskContext` contains `Task` clone**: Currently `TaskContext` holds `task: Task`. After extraction, this needs to become generic or be refactored to hold TaskNode-accessible data. Moving `build_task_context` into `TaskStore` (Phase 2b) helps — the store knows the concrete Task type.
