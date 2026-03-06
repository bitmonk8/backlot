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

See [README.md](../README.md) for tech stack (Dependencies) and module structure.

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
