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
│  - Research    │  - All calls via Flick lib  │
│  - Verification│  - Per-call model selection │
│  - Document    │  - Per-call tool scoping    │
│    Store       │  - Prompt assembly          │
│                │                            │
├────────────────┴────────────────────────────┤
│  Infrastructure                             │
│  (git operations, state persistence,        │
│   event system, metrics, configuration)     │
└─────────────────────────────────────────────┘
```

## Tech Stack

| Concern | Choice | Crate(s) |
|---|---|---|
| Async runtime | tokio | `tokio` |
| Error handling | thiserror at module boundaries, anyhow for propagation | `thiserror`, `anyhow` |
| Serialization | serde ecosystem | `serde`, `serde_json`, `toml` |
| Agent runtime | Flick (library crate dependency) | `flick` (git dependency) |
| TUI | ratatui + crossterm, read-only monitoring for v1 | `ratatui`, `crossterm` |
| Config format | TOML | `toml` |

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
│   ├── tools.rs             # Tool access flags
│   ├── models.rs            # Model selection and escalation
│   └── prompts.rs           # Prompt templates and assembly
├── services/
│   ├── research.rs          # Research service (DocumentStore + exploration)
│   ├── verification.rs      # Build/lint/test execution
│   └── document_store.rs    # File-based (markdown) knowledge store; librarian via Flick agent
├── tui/
│   ├── mod.rs               # TUI application (read-only monitoring for v1)
│   ├── task_tree.rs         # Task tree widget
│   ├── worklog.rs           # Worklog panel (event-level updates, no token streaming)
│   └── metrics.rs           # Metrics display
├── config/
│   ├── mod.rs               # Configuration loading (TOML; ~/.config/epic/config.toml + project epic.toml)
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
- `TaskContext` and `FlickAgent` — bundle Flick library configuration, document store, verification config. Each agent call creates a new `FlickClient` via Flick's library API (stateless per-call, no process spawning).
- `EventEmitter` — trait object for logging/TUI events
- `ProjectConfig` — verification steps, paths, model preferences (loaded from TOML)
- `EpicState` — task tree and session state (owned by orchestrator)
