# TUI Design

## Purpose

Real-time visibility into epic execution: task tree, agent output, progress, and metrics. Users watch the orchestrator work and can understand what's happening at any point.

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
│    ✓ Sub-B         │      [agent output streaming]      │
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
- Agent text output (streaming)
- Tool calls (summarized)
- Verification results (pass/fail per step)
- Discovery notifications
- Error/fix loop iterations

## Metrics Panel (Toggle)

Token usage per model tier, session cost, task count (completed/total).

## Rust TUI Framework

Candidate: `ratatui` — mature Rust TUI framework with async support. Alternatives: `cursive`, `tui-rs` (predecessor to ratatui).

## Event System

The orchestrator emits events consumed by the TUI:
- `TaskStarted { id, goal }`
- `PhaseChanged { task_id, phase }`
- `AgentOutput { task_id, text }` (streaming)
- `VerificationResult { task_id, step, passed }`
- `TaskCompleted { id, outcome }`
- `DiscoveryRecorded { task_id, summary }`
- `MetricsUpdated { tokens, cost }`

Events also feed file logging (structured JSONL) for post-run analysis.
