# Task Model — Recursive Problem-Solver

Based on EPIC_DESIGN2 (`C:\UnitySrc\fds2\EPIC_DESIGN2.md`). This document adapts the design for Rust implementation.

## Core Concept

Every task is the same type: a **problem-solver**. There is one task type with two execution paths determined at runtime.

```
1. ASSESS   — Estimate complexity, select path (leaf/branch) and model
2. EXECUTE  — Solve directly (leaf) or decompose and delegate (branch)
3. VERIFY   — Independent verification of the result
```

## Task State

```rust
struct Task {
    id: TaskId,
    parent_id: Option<TaskId>,
    goal: String,
    verification_criteria: Vec<String>,
    path: Option<TaskPath>,         // None before assessment, then Leaf or Branch
    phase: TaskPhase,               // Pending → Assessing → Executing → Verifying → Completed | Failed
    model: Option<Model>,           // Selected by assessment
    current_model: Option<Model>,   // Differs from model after escalation
    attempts: Vec<Attempt>,         // Retry/escalation history
    subtask_ids: Vec<TaskId>,       // Empty for leaves
    magnitude_estimate: Option<MagnitudeEstimate>,  // Set by parent for leaves
    discoveries: Vec<String>,       // Context propagation summaries
    decomposition_rationale: Option<String>,         // Branch only
    depth: u32,                     // Root = 0, depth cap at 8
}
```

## Assessment

Single Haiku call returns `{path, model, rationale}`. Two orthogonal decisions:
- **Path**: leaf (solve directly) or branch (decompose)
- **Model**: which model executes the work

Tie-breaking bias: branch. Recovery from wrong-branch is cheaper than wrong-leaf.

If Haiku is uncertain about model, conditional escalation to Sonnet for a second assessment.

Root task is always forced to branch (guarantees recovery machinery exists).

## Leaf Path

1. Implement (model chosen by assessment: Haiku/Sonnet/Opus)
   - Research Service available as a tool
   - Structured output via `submit_result` tool (see [Agent Design](AGENT_DESIGN.md))
2. Verification gates — configurable per-project via `epic.toml` (build, lint, test, etc.)
3. File-level review — model: max(Haiku, implementing model), capped at Sonnet
4. Local simplification review — same model as file-level review
5. Fix loop on failure (3 retries per model tier, then escalate)
6. Commit on success, full rollback on terminal failure

## Branch Path

1. Design + Decompose in same context (single level, 2-5 subtasks)
   - Research Service available as a tool
   - Decomposition strategy chosen per-problem (structural, behavioural, goal-based)
2. Create subtasks with magnitude estimates
3. Execute subtasks sequentially (DFS preorder)
4. Inter-subtask checkpoint on discoveries
5. Branch verification (Sonnet): correctness + completeness + aggregate simplification
6. Up to 3 fix rounds; root gets one additional Opus round

## Recovery

Ordered cheapest → most expensive:
1. Scope circuit breaker (3x magnitude estimate → immediate rollback)
2. Retry budget exhaustion → model escalation (Haiku→Sonnet→Opus)
3. Terminal leaf failure → rollback, fail to parent
4. Parent Opus recovery assessment (max `max_recovery_rounds` recovery rounds per branch, default 2, configurable via `epic.toml`)
5. Branch failure → escalate to grandparent
6. Global task count cap (`max_total_tasks`, default 100) — hard limit on total tasks created in a single run, preventing unbounded cost growth from recovery amplification

## Context Propagation

Two channels:
- **Task metadata** (small, injected): goal, criteria, discovery summaries
- **DocumentStore** (large, queried on demand): full research, analysis, failure records

Structural map injection per agent call:
- Own task: goal, criteria
- Parent: goal, decomposition rationale, discoveries
- Ancestor chain: compressed one-line summaries
- Completed siblings: goal, outcome, discovery summaries
- Pending siblings: goal only

## Discovery Flow

1. Agent discovers reality differs from assumptions
2. Records full detail in DocumentStore
3. Records 1-3 sentence summary in own task's `discoveries`
4. Parent runs inter-subtask checkpoint (Haiku classification):
   - **proceed**: no impact
   - **adjust**: modify pending subtasks (branch's own model)
   - **escalate**: Opus recovery (decomposition strategy invalid)
5. If discovery affects parent scope, parent records own discovery → bubbles up

## Depth Cap

Depth 8 (configurable). Tasks at max depth forced to leaf path. Catches runaway decomposition.
