# TUI Design

## Purpose

Real-time visibility into epic execution: task tree, agent output, progress, and metrics. Users watch the orchestrator work and can understand what's happening at any point. **Read-only monitoring for v1** — orchestrator emits events, TUI consumes. Decoupled architecture supports adding interactive controls later.

## Layout

```
┌──────────────────────────────────────────────────────────┐
│  Branch: epic/slug   Problem: "..."   Cost: $X.XX        │
├────────────────────┬─────────────────────────────────────┤
│  Task Tree         │  Worklog                            │
│                    │                                     │
│  ▶ Root problem    │  → Assess ... ✓ [2s]               │
│    ✓ Sub-A         │  → Design + Decompose ... ✓ [45s]  │
│      ✓ A.1         │  → Execute subtask C.1             │
│      ✓ A.2         │    → Implement ...                 │
│    ✓ Sub-B         │      [agent output events]         │
│    ▸ Sub-C         │                                     │
│      ▸ C.1 ←       │                                     │
│        C.2         │                                     │
│                    │                                     │
├────────────────────┴─────────────────────────────────────┤
│  q: quit  t: tail  m: metrics                            │
└──────────────────────────────────────────────────────────┘
```

## Task Tree Indicators

```
  ✓  Completed
  ▸  In progress (current)
  ✗  Failed
  ·  Pending
  ←  Currently executing task
```

Indentation shows parent-child hierarchy. Since tasks are uniform (no type distinction), the tree shows goal text and status only.

## Worklog

Streams agent output and phase transitions for the current task:
- Phase start/end with duration
- Agent text output (event-level, no token streaming in v1)
- Tool calls (summarized)
- Verification results (pass/fail per step)
- Discovery notifications
- Error/fix loop iterations

## Metrics Panel (Toggle)

Token usage per model tier, session cost, task count (completed/total).

## Rust TUI Framework

`ratatui` with `crossterm` backend. De facto Rust TUI framework, async-compatible with tokio, actively maintained successor to tui-rs.

## Event System

The orchestrator emits events consumed by the TUI:
- `TaskRegistered { task_id, parent_id, goal, depth }`
- `PhaseTransition { task_id, phase }`
- `PathSelected { task_id, path }`
- `ModelSelected { task_id, model }`
- `ModelEscalated { task_id, from, to }`
- `SubtasksCreated { parent_id, child_ids }`
- `TaskCompleted { task_id, outcome }`
- `RetryAttempt { task_id, attempt, model }`
- `VerificationStarted { task_id }`
- `VerificationComplete { task_id, passed }`
- `DiscoveriesRecorded { task_id, count }`
- `CheckpointAdjust { task_id }`
- `CheckpointEscalate { task_id }`
- `FixAttempt { task_id, attempt, model }`
- `FixModelEscalated { task_id, from, to }`
- `BranchFixRound { task_id, round, model }`
- `FixSubtasksCreated { task_id, count, round }`
- `RecoveryStarted { task_id, round }`
- `RecoveryPlanSelected { task_id, approach }`
- `RecoverySubtasksCreated { task_id, count, round }`
- `TaskLimitReached { task_id }`

Events also feed file logging (structured JSONL) for post-run analysis.
