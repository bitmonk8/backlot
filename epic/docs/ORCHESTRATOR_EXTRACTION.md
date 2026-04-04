# Orchestrator Extraction Spec

## Goal

Extract the orchestrator (recursive problem-solver coordination engine) from epic into a standalone sibling crate. The orchestrator becomes a generic framework for recursive task decomposition with retry, escalation, fix loops, and recovery. Epic provides the concrete task implementation (AI agent calls, prompts, wire formats) through traits defined by the orchestrator crate.

**Key constraint**: Epic's `Task` struct, its lifecycle methods (`execute_leaf`, `verify_branch`, etc.), and all AI-specific logic stay in epic. The orchestrator crate defines the coordination algorithm and the trait contract that tasks must satisfy.

Crate name: **cue**.

---

## Architecture

```
cue (orchestrator crate)
  - Defines: TaskNode trait, TaskStore trait, coordination loop, state machine, events, tree context
  - Generic over: S: TaskStore

epic (application crate)
  - Defines: Task struct (implements cue::TaskNode), EpicState (implements cue::TaskStore)
  - Defines: AgentService trait, ReelAgent, Services (runtime deps injected into tasks)
  - Owns: CLI, TUI, prompts, wire formats, knowledge/research, config shell
  - Depends on: cue, reel, vault, lot
```

The orchestrator never constructs tasks directly — it calls `TaskStore` methods to create them. It never reads task fields directly — it calls trait accessors. It never performs AI operations — tasks receive runtime deps (agent, vault) at construction time and call them internally. The orchestrator has no knowledge of `AgentService` or any AI-specific types.

---

## Trait Design

The orchestrator's coupling to Task is deep: ~15 field reads, ~12 mutation methods, ~8 async lifecycle methods, plus task construction. This must be captured in traits.

### Approach: Two-Trait Split

**`TaskNode`** — Data access, decisions, mutations, and lifecycle. Implemented by epic's `Task`.

**`TaskStore`** — Task creation, storage, lookup, cross-task queries, tree context building, and runtime re-injection after resume. Implemented by epic's `EpicState`.

The lifecycle methods currently on Task (`execute_leaf`, `verify_branch`, etc.) call the agent service internally using runtime deps injected at task construction time. They stay in epic as `TaskNode` method implementations. The orchestrator has no knowledge of `AgentService` or any AI-specific types.

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
// Tasks receive runtime deps (agent, vault, event sender) at construction time
// via TaskStore::create_subtask / TaskStore::bind_runtime. No service parameter needed.

async fn execute_leaf(&mut self, ctx: &TreeContext) -> TaskOutcome;

async fn verify_branch(&mut self, ctx: &TreeContext)
    -> BranchVerifyOutcome;   // includes FailedNoFixLoop for fix tasks

fn fix_round_budget_check(&self, limits: &LimitsConfig)
    -> FixBudgetCheck;        // no is_root param; task knows internally

async fn check_branch_scope(&self) -> ScopeCheck;

async fn design_fix(&mut self, ctx: &TreeContext,
    failure_reason: &str, round: u32, model: Model)
    -> Result<DecompositionResult>;

async fn handle_checkpoint(&mut self, ctx: &TreeContext,
    discoveries: &[String])
    -> ChildResponse;

fn can_attempt_recovery(&self, limits: &LimitsConfig)
    -> RecoveryEligibility;   // collapses is_fix_task + budget + round

async fn assess_and_design_recovery(&mut self, ctx: &TreeContext,
    failure_reason: &str, round: u32)
    -> RecoveryDecision;
```

**Trait surface**: 8 read accessors, 6 decision methods, 6 mutations, 8 lifecycle methods = **28 methods total**.

Without collapsing: 16 read accessors, 9 mutations, 8 lifecycle methods = **33 methods total**, and the coordinator contains decision logic that belongs to the task.

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

    // --- Runtime injection (called once after deserialization on resume) ---
    fn bind_runtime(&mut self);

    // --- Task creation (replaces Task::new + field mutation in coordinator) ---
    fn create_subtask(&mut self, parent_id: TaskId, spec: &SubtaskSpec,
        mark_fix: bool, inherit_recovery_rounds: Option<u32>) -> TaskId;

    // --- Cross-task queries (moved from coordinator) ---
    fn any_non_fix_child_succeeded(&self, parent_id: TaskId) -> bool;

    // --- Tree context building (moved from orchestrator/context.rs) ---
    fn build_tree_context(&self, id: TaskId) -> Result<TreeContext, OrchestratorError>;
}
```

Epic's `EpicState` implements `TaskStore` with `type Task = Task`.

Key changes from the current design:
- **`bind_runtime`** re-injects non-serializable runtime deps (agent, vault, event sender) into all tasks after deserializing `state.json` on resume. The store holds the shared runtime deps and iterates its task map. Not parameterized — the store implementation knows its own runtime types.
- **`create_subtask`** replaces the coordinator's `Task::new()` + field mutation sequence. The store implementation (in epic) constructs the concrete Task from the SubtaskSpec and injects runtime deps. Coordinator no longer needs to know Task's constructor signature or mutable fields.
- **`any_non_fix_child_succeeded`** absorbs the cross-task guard that currently reads `is_fix_task` + `phase` on each child. Removes `is_fix_task` from the TaskNode accessor surface.
- **`build_tree_context`** moves from `orchestrator/context.rs` into TaskStore. This is a tree-level query that accesses Task fields through the store's internal `HashMap`, not through the trait. This eliminates the need for TreeContext building to use TaskNode accessors (it uses concrete Task fields internally). `build_task_context` is not on the trait — `TaskContext` stays in epic and is built by Task internally from `TreeContext` + `&self`.

### Runtime Dependency Injection

Tasks need runtime deps (agent, vault, event sender) for lifecycle methods. These are injected at construction time, not passed by the orchestrator.

**Construction path**: `TaskStore::create_subtask()` is implemented by epic's `EpicState`, which holds shared runtime deps (`Arc<ReelAgent>`, `Option<Arc<Vault>>`, `EventSender`). When creating a task, EpicState injects these into the `Task` struct.

**Resume path**: Runtime deps are not serializable. After deserializing `state.json`, epic calls `TaskStore::bind_runtime()` to re-inject deps into all existing tasks. `bind_runtime()` is called once at startup before the orchestrator runs.

```rust
// In epic — Task struct (not part of orchestrator crate)
struct Task {
    // ... serialized fields ...
    #[serde(skip)]
    runtime: Option<Arc<TaskRuntime>>,  // injected at construction or bind
}

struct TaskRuntime {
    agent: ReelAgent,
    vault: Option<Arc<vault::Vault>>,
    events: EventSender,
    project_root: PathBuf,
}
```

The orchestrator never sees `TaskRuntime`, `ReelAgent`, or `Vault`. It calls `task.execute_leaf(ctx)` and the task uses its own runtime deps internally.

**Orchestrator's own needs**: The orchestrator needs to emit events (task registration, phase transitions) and check limits (task count cap). These are injected into the `Orchestrator` at construction:

```rust
// In orchestrator crate
pub struct Orchestrator<S: TaskStore> {
    store: S,
    events: EventSender,
    limits: LimitsConfig,
    state_path: Option<PathBuf>,
}
```

### What the Orchestrator Crate Defines

| Category | Types |
|---|---|
| Core identity | `TaskId` |
| State machine | `TaskPhase`, `TaskPath`, `Model` |
| Decision enums | `BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `ChildResponse`, `CheckpointDecision`, `ScopeCheck`, `RecoveryEligibility`, `ResumePoint` |
| Results | `TaskOutcome`, `LeafResult`, `RecoveryPlan`, `DecompositionResult`, `SubtaskSpec`, `AssessmentResult`, `VerificationOutcome`, `VerificationResult`, `VerifyOutcome` |
| Data | `Attempt`, `Magnitude`, `MagnitudeEstimate`, `TaskUsage`, `SessionMeta`, `AgentResult<T>` |
| Context | `TreeContext`, `SiblingSummary`, `ChildStatus`, `ChildSummary` |
| Infrastructure | `EventSender`, `EventReceiver`, `Event`, `event_channel()` |
| Config | `LimitsConfig`, `VerificationStep` |
| Traits | `TaskNode`, `TaskStore` |
| Coordinator | `Orchestrator<S: TaskStore>`, `OrchestratorError` |

### What Epic Defines

| Category | Content |
|---|---|
| Task impl | `Task` struct, `TaskRuntime`, `impl TaskNode for Task`, all lifecycle method bodies |
| State impl | `EpicState`, `impl TaskStore for EpicState`, `bind_runtime()` |
| Agent layer | `AgentService` trait, `ReelAgent`, `Services` (runtime deps bundle) |
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
| `orchestrator/mod.rs` | Coordinator logic (now generic over `S: TaskStore`) |
| `orchestrator/context.rs` | `TreeContext` struct (build logic moves to `TaskStore`) |
| `agent/mod.rs` (partial) | `SessionMeta`, `AgentResult<T>`, `SiblingSummary`, `ChildStatus`, `ChildSummary` |
| `events.rs` | `Event`, `EventSender`, `EventReceiver`, `event_channel()` |
| `config/project.rs` (partial) | `LimitsConfig`, `VerificationStep` |

Plus new trait definitions: `TaskNode`, `TaskStore`.

Plus types currently in task/ that are part of the orchestration protocol (not task-specific): `TaskId`, `TaskPhase`, `TaskPath`, `Model`, `TaskOutcome`, `Attempt`, `Magnitude`, `MagnitudeEstimate`, `TaskUsage`, `LeafResult`, `RecoveryPlan`, `AssessmentResult`, `SubtaskSpec`, `DecompositionResult`, `CheckpointDecision`, `BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `RecoveryEligibility`, `ResumePoint`, `ChildResponse`, `ScopeCheck`, `VerificationOutcome`, `VerificationResult`, `VerifyOutcome`.

### What Stays in Epic

| Current location | Content | Why |
|---|---|---|
| `task/mod.rs` | `Task` struct, `TaskRuntime`, `Task::new()`, mutation method bodies | Concrete task implementation |
| `task/leaf.rs` | `Task::execute_leaf()` and helpers | AI-specific lifecycle logic |
| `task/branch.rs` | Branch `Task` methods | AI-specific branch decisions |
| `task/scope.rs` | `git_diff_numstat`, `evaluate_scope` | Called from Task methods which stay in epic |
| `state.rs` | `EpicState` struct + persistence + `bind_runtime()` | Concrete state implementation |
| `main.rs` | CLI entry point | Application shell |
| `cli.rs`, `init.rs` | CLI commands | Application shell |
| `tui/` | TUI rendering | Presentation layer |
| `sandbox.rs` | Container detection | Startup check |
| `agent/mod.rs` (partial) | `AgentService` trait | Epic-specific (tasks call agents, not orchestrator) |
| `agent/reel_adapter.rs` | `ReelAgent` | Concrete agent |
| `agent/prompts.rs` | Prompt templates | Epic-specific |
| `agent/wire.rs` | Wire format types | Epic-specific |
| `knowledge.rs` | Research service | Epic-specific |
| `config/project.rs` (partial) | `EpicConfig`, `ModelConfig`, `VaultConfig` | Epic-specific config |
| `orchestrator/services.rs` | `Services<A>` | Replaced by runtime injection; no longer needed |
| `test_support.rs` | `MockAgentService`, `MockBuilder` | See challenges section |

---

## Dependency Analysis

### Vault Integration

The orchestrator currently calls `vault::Vault::record()` and `vault::Vault::reorganize()` directly in `orchestrator/mod.rs`.

With runtime injection, most vault calls already route through Task lifecycle methods (leaf discovers → records to vault). The orchestrator's direct vault calls are:
- `record()` for verification failures and checkpoint decisions (in coordinator code)
- `reorganize()` after root branch children complete

These coordinator-level vault operations need to be pushed into TaskNode or TaskStore methods so the orchestrator has no vault dependency. Options:

- **(A) Push into TaskNode.** Add methods like `record_verification_failure(reason)`, `on_branch_children_complete()` to TaskNode. Task calls vault internally.
- **(B) Push into TaskStore.** TaskStore gets `record_for_task(id, ...)` and `reorganize()`. EpicState holds the vault reference and delegates.

**Recommendation**: **(A)**. The coordinator-level vault calls are always in the context of a specific task. Pushing them into TaskNode keeps the pattern consistent — the task owns all side effects, the orchestrator just coordinates.

### Scope Circuit Breaker

`task/scope.rs` contains `git_diff_numstat()` (shells out to git) and `evaluate_scope()` (pure comparison). Currently called by Task lifecycle methods.

**Options**:
- Move to orchestrator crate (it's a reusable coordination mechanism)
- Keep in epic (it's called from Task methods which stay in epic)

**Recommendation**: Keep in epic. The Task lifecycle methods call it, and those stay in epic. The orchestrator doesn't call scope functions directly — it calls `task.check_branch_scope()` which is a TaskNode method.

### TreeContext Building

`build_tree_context()` reads ~10 task fields to construct context. Currently a free function in `orchestrator/context.rs` using direct field access on `Task`.

This becomes a `TaskStore` method (see Phase 2b). The store implementation accesses tasks through its internal `HashMap` using concrete `Task` fields, not through `TaskNode` trait accessors. This keeps the build logic simple and eliminates a large accessor surface from `TaskNode`.

The `TreeContext` struct itself moves to the orchestrator crate (it's part of the coordination protocol). The build logic lives in epic's `TaskStore` implementation.

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

`build_context` (which combines TreeContext + task clone into TaskContext) stays in epic — `TaskContext` is an epic-internal type built by Task from `TreeContext` + `&self`.

#### 2c. Move `create_subtasks` task-construction logic into EpicState

Coordinator's `create_subtasks` (lines 327-384) calls `Task::new()` + sets fields + inserts. Move the Task-constructing portion to `EpicState::create_subtask(parent_id, spec, mark_fix, inherit_recovery_rounds) -> TaskId`. Coordinator keeps the event emission and task-limit checking. Eliminates coordinator knowing Task's constructor.

### Phase 3: Decouple types into separable files

No behavioral changes — just file organization to make the extraction cut mechanical.

#### 3a. Extract context types from `agent/mod.rs`

Move `TaskContext`, `SessionMeta`, `AgentResult<T>`, `SiblingSummary`, `ChildStatus`, `ChildSummary` to a separate file (these move to the orchestrator crate). `AgentService` stays in `agent/mod.rs` — it is not extracted.

#### 3b. Extract orchestration-protocol types from `task/mod.rs`

Move `TaskId`, `TaskPhase`, `TaskPath`, `Model`, `Attempt`, `Magnitude`, `MagnitudeEstimate`, `TaskUsage`, `TaskOutcome`, `LeafResult`, `RecoveryPlan` to `src/task/types.rs`. `task/mod.rs` re-exports.

#### 3c. Extract orchestration-protocol types from `task/branch.rs`

Move `SubtaskSpec`, `DecompositionResult`, `CheckpointDecision`, `BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `ChildResponse` to their own file. `branch.rs` re-exports.

#### 3d. Split `LimitsConfig` / `VerificationStep` from config

Move to `src/config/limits.rs`. `EpicConfig` imports them.

#### 3e. Push coordinator-level vault calls into TaskNode

Move the orchestrator's direct vault calls (`record` for verification failures/checkpoint decisions, `reorganize` after root branch children complete) into TaskNode methods. Task handles vault internally via its injected runtime deps. Eliminates the orchestrator's vault dependency without a `DocumentStore` trait.

#### 3f. Deduplicate verify/scope helpers

Consolidate the duplicated `record_to_vault`, `emit_usage_event`, `try_verify`, `try_file_level_review`, `verification_model`, `check_scope` methods.

### Phase 4: Define traits

With decisions pushed in and types separated, define `TaskNode` and `TaskStore` traits. Implement for `Task` and `EpicState`. Make `Orchestrator` generic.

This is the final prep step. At this point the extraction boundary is clean and the mechanical extraction (Phase 5) is trivial.

### Phase 5: Verify clean boundary

Audit that the code destined for the orchestrator crate has no imports from: `tui`, `cli`, `init`, `sandbox`, `knowledge`, `reel_adapter`, `prompts`, `wire`, `vault`, `reel`, `flick`, `lot`, `AgentService`.

### Phase 6: Mechanical extraction

Move files to new crate, update import paths, publish, add git dependency.

---

## New Crate Structure

```
cue/
├── Cargo.toml                # workspace root
├── cue/                       # library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # Public API re-exports
│       ├── traits.rs          # TaskNode, TaskStore
│       ├── types.rs           # TaskId, TaskPhase, TaskPath, Model, Attempt, etc.
│       ├── context.rs         # TaskContext, TreeContext, SiblingSummary, etc.
│       ├── events.rs          # Event, EventSender, EventReceiver
│       ├── config.rs          # LimitsConfig, VerificationStep
│       ├── orchestrator.rs    # Coordinator (Orchestrator<S: TaskStore>)
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

`MockAgentService` (648 lines) implements `AgentService`. Since `AgentService` stays in epic (not extracted), the mock stays in epic unchanged. The orchestrator crate needs its own test infrastructure: a mock `TaskNode` and mock `TaskStore` to test the coordination algorithm in isolation. These are simpler — they just return canned decision enums and outcomes.

### 2. Async trait methods

`TaskNode` has async lifecycle methods. Rust 2024 edition supports `async fn` in traits with `impl Future` return types (RPITIT). This works if the trait is not object-safe. If object safety is needed later, `async-trait` or manual boxing would be required.

The orchestrator is generic: `Orchestrator<S: TaskStore>`. Single type parameter — clean.

### 3. State file compatibility

`EpicState` serialization includes `Task` (with all its fields). The `Task` struct stays in epic. The orchestrator-protocol types (`TaskId`, `TaskPhase`, etc.) move to the orchestrator crate but their serde representation is unchanged. Backward compatibility maintained.

### 4. Task construction

Addressed by prep Phase 2c: `EpicState::create_subtask()` absorbs the `Task::new()` + field mutation + insert sequence. Coordinator just gets back a `TaskId`.

---

## Open Questions

None.

## Resolved Questions

- **Crate name**: `cue`.
- **AgentService location**: Stays in epic. The orchestrator never calls agents — tasks receive runtime deps at construction and call agents internally. No `AgentService` trait in the orchestrator crate.
- **Services<A>**: Eliminated. Replaced by runtime dependency injection at task construction time, with `TaskStore::bind_runtime()` for re-injection on resume.
- **Type parameter ergonomics**: Resolved — `Orchestrator<S: TaskStore>` (single param). `TaskStore` has `type Task: TaskNode` as an associated type.
- **DocumentStore trait**: Not needed. Vault calls pushed into TaskNode methods — orchestrator has no vault dependency.
- **`TaskContext` contains `Task` clone**: `TaskContext` stays in epic. The orchestrator only passes `TreeContext` to TaskNode lifecycle methods. Epic's Task builds `TaskContext` internally from `TreeContext` + `&self` when making agent calls. `build_task_context` is not on the `TaskStore` trait.
