# Architecture

## System Layers

```
┌─────────────────────────────────────────────┐
│  CLI / TUI                                  │
│  (argument parsing, interactive capture,    │
│   task tree display, worklog, progress)     │
├─────────────────────────────────────────────┤
│  Orchestrator                               │
│  (recursive task execution, DFS traversal,  │
│   state persistence, resume)                │
├─────────────────────────────────────────────┤
│  Task Engine                                │
│  (ProblemSolverTask: assess → execute       │
│   → verify, leaf/branch paths)              │
├────────────────┬────────────────────────────┤
│  Services      │  Agent Layer               │
│  - Research    │  - ZeroClaw integration     │
│  - Verification│  - Model selection          │
│  - Document    │  - Tool access control      │
│    Store       │  - Prompt assembly          │
├────────────────┴────────────────────────────┤
│  Infrastructure                             │
│  (git operations, state persistence,        │
│   event system, metrics, configuration)     │
└─────────────────────────────────────────────┘
```

## Module Structure (Preliminary)

```
src/
├── main.rs                  # Entry point, CLI parsing
├── orchestrator.rs          # Recursive task execution
├── task/
│   ├── mod.rs               # ProblemSolverTask definition
│   ├── assess.rs            # Assessment (path + model selection)
│   ├── leaf.rs              # Leaf execution path
│   ├── branch.rs            # Branch execution path (decompose + delegate)
│   └── verify.rs            # Verification (leaf and branch variants)
├── agent/
│   ├── mod.rs               # Agent abstraction
│   ├── zeroclaw.rs          # ZeroClaw integration
│   ├── tools.rs             # Tool access flags
│   ├── models.rs            # Model selection and escalation
│   └── prompts.rs           # Prompt templates and assembly
├── services/
│   ├── research.rs          # Research service (DocumentStore + exploration)
│   ├── verification.rs      # Build/lint/test execution
│   └── document_store.rs    # Centralized knowledge management
├── tui/
│   ├── mod.rs               # TUI application
│   ├── task_tree.rs         # Task tree widget
│   ├── worklog.rs           # Worklog panel
│   └── metrics.rs           # Metrics display
├── config/
│   ├── mod.rs               # Configuration loading
│   └── project.rs           # Per-project verification config
├── git.rs                   # Git operations (commit, rollback, diff)
├── state.rs                 # EpicState persistence and resume
├── events.rs                # Event system
└── metrics.rs               # Token/cost tracking
```

## Data Flow

```
User input (problem description)
  │
  ├─ Interactive capture → Requirements document
  │
  ▼
Orchestrator creates root task
  │
  ▼
Root task: ASSESS → always branch
  │
  ├─ Design + Decompose → subtasks
  │
  ▼
For each subtask (DFS preorder):
  │
  ├─ ASSESS (Haiku) → leaf or branch?
  │
  ├─ LEAF: implement → fix loop → verify → commit
  │   └─ On failure: escalate model, then fail to parent
  │
  ├─ BRANCH: design + decompose → execute children → verify aggregate
  │   └─ On child failure: Opus recovery assessment
  │
  └─ Inter-subtask checkpoint (if discoveries)
      └─ proceed / adjust / escalate
```

## Dependency Injection

All major components receive their dependencies explicitly. No globals, statics, or singletons. The entry point constructs the dependency graph and threads it through.

Key dependency types:
- `AgentContext` — bundles agent factory, document store, verification config
- `EventEmitter` — trait object for logging/TUI events
- `ProjectConfig` — verification steps, paths, model preferences
- `EpicState` — task tree and session state (owned by orchestrator)
