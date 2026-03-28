# epic

AI orchestrator that decomposes large software engineering tasks into smaller problems, delegates them to AI agents, and assembles verified results.

## Overview

Epic manages AI agent sessions to implement complex features through a recursive problem-solving model. Each task is assessed, executed (directly or via subtask decomposition), and verified. Unlike waterfall approaches that plan everything upfront, epic adapts as execution reveals new information.

**Core principle**: You cannot extract information from a system without affecting it. Each exploration, implementation attempt, and test run changes what we know. Epic embraces this by reassessing and recovering as it learns.

## Design Philosophy

> "No plan of operations reaches with any certainty beyond the first encounter with the enemy's main force."
> — Helmuth von Moltke, 1871

Software development is fundamentally about managing uncertainty. Each action produces information that changes the situation. The optimal next step depends on information not yet gathered.

Epic operationalizes this through:
- **Recursive decomposition**: Every task follows the same pattern — assess, execute, verify — whether it's a leaf (direct work) or a branch (subtask decomposition)
- **Adaptive recovery**: When a child task fails, an Opus recovery agent creates new subtasks informed by what was learned
- **Checkpoint guidance**: After each child completes, a checkpoint classifies the result (proceed/adjust/escalate) and propagates guidance to subsequent siblings
- **Depth-first execution**: Complete subtrees before siblings — simple mental model, coherent output

## Architecture

### Recursive Problem-Solver

Every task follows the same lifecycle:

```
Assess (Haiku) → Leaf or Branch?
  Leaf:   Execute (Sonnet/Opus) → Verify → Done
  Branch: Decompose (model from assessment) → Execute children (DFS) → Verify → Done
```

There are no specialized task types (no RESEARCH, DESIGN, PLAN, etc.). The assessment agent classifies each task as leaf or branch and selects the appropriate model tier. This simplifies the system while preserving expressiveness — the agent decides what kind of work a task requires.

### Model Selection

Three model tiers with escalation on failure:

| Tier | Default Model | Role |
|------|---------------|------|
| Haiku | claude-haiku-4-5-20251001 | Assessment, checkpoint classification |
| Sonnet | claude-sonnet-4-6 | Leaf execution, decomposition, verification |
| Opus | claude-opus-4-6 | Recovery assessment, escalation target |

Leaf execution retries at each tier before escalating (Haiku → Sonnet → Opus). Model names are configurable via `epic.toml`.

### Tool Access

Six tools with phase-based access control via `ToolGrant` bitflags (WRITE, NU). Tool definitions and execution are provided by the reel crate:

| Tool | Grant Required | Description |
|------|----------------|-------------|
| `Read` | — | Read file contents (max 256 KiB) |
| `Glob` | — | Find files by glob pattern (max 1000 results) |
| `Grep` | — | Search file contents by regex (max 64 KiB output) |
| `Write` | WRITE | Create or overwrite a file |
| `Edit` | WRITE | Replace exact substring in a file |
| `NuShell` | NU | Execute NuShell command via persistent MCP session (timeout: 120s default, 600s max) |

**Phase → tool grants:**
- Analyze: NU
- Execute: WRITE + NU
- Decompose: NU

Read, Glob, and Grep require no grant and are available in all phases. Path containment is enforced by the lot sandbox.

### Verification

Every task is verified after execution. Verification commands are configured per-project in `epic.toml`:

```toml
[[verification]]
name = "build"
command = ["cargo", "build"]

[[verification]]
name = "test"
command = ["cargo", "test"]

[[verification]]
name = "lint"
command = ["cargo", "clippy", "--", "-D", "warnings"]
```

After verification gates pass for leaf tasks, a **file-level review** checks that source file changes align with the task's goal and verification criteria. This catches intent/requirement mismatches that build/lint/test cannot detect. Review failures feed into the fix loop.

On verification or review failure, fix loops attempt to repair the work:
- **Leaf fix loop**: Retry with model escalation (Sonnet → Opus), scope circuit breaker prevents runaway changes
- **Branch fix loop**: Opus agent designs targeted fix subtasks (3 rounds for non-root, 4 for root)

### Recovery

When a child task fails within a branch, epic attempts recovery:

1. Opus assesses whether the failure is recoverable
2. If recoverable, Opus designs recovery subtasks informed by what was learned
3. Two approaches: **incremental** (preserve completed work, append recovery tasks) or **full re-decomposition** (replace remaining pending siblings)
4. Max 2 recovery rounds per branch (budget inherited by nested branches to prevent exponential growth)

### Knowledge Store and Research Service

Epic maintains a persistent knowledge base at `.epic/docs/` via the vault crate. The vault stores project knowledge, discoveries, and design decisions accumulated across agent sessions.

The `ResearchQuery` tool is available to agents during execution and decomposition phases. When called, it runs a multi-step pipeline:

1. **Query vault** for existing knowledge
2. **Identify gaps** in coverage (Haiku, structured output)
3. **Explore the codebase** to fill gaps (Haiku with read-only tools)
4. **Synthesize** a final answer combining vault knowledge and exploration findings

An optional `scope` parameter controls behavior: `vault` (stored knowledge only) or `project` (default, vault + codebase exploration). Exploration findings are recorded back into the vault for future queries.

### Scope Circuit Breaker

Each task carries a magnitude estimate (small/medium/large) mapping to expected line counts. Before verification, `git diff --numstat` is compared against 3x the estimate. If the actual change exceeds the threshold, the task is failed to prevent unbounded scope creep.

### State Persistence

Task state is persisted to `.epic/state.json` with atomic writes (write to temp, rename). Checkpoints occur after assessment, decomposition, child completion, and verification. On crash, `epic resume` continues from the last checkpoint.

### Event System

The orchestrator emits events for every state change (24 event types). The TUI consumes these to render a live task tree, worklog, and metrics panel. In headless mode, events are printed to stderr.

## Usage

### Initialize a Project

```bash
epic init
```

A Sonnet agent scans the project for build system markers, test frameworks, and linters. An interactive CLI confirms each detected verification step, prompts for model preferences and depth/budget limits, then writes `epic.toml`.

### Start a Run

```bash
epic run "implement feature X"
```

Decomposes the goal into subtasks, executes them depth-first, and verifies results. If a state file exists with the same goal, resumes transparently.

### Resume an Interrupted Run

```bash
epic resume
```

Loads `.epic/state.json` and continues from the last checkpoint. Completed and failed tasks are skipped. In-progress tasks restart from the appropriate phase.

### Check Status

```bash
epic status
```

Prints the goal, root task phase, and task counts (completed/in-progress/pending/failed) from persisted state. No agent or API calls needed.

### Global Options

| Option | Env Var | Default | Description |
|--------|---------|---------|-------------|
| `--credential <NAME>` | `EPIC_CREDENTIAL` | `anthropic` | Credential name for provider resolution |
| `--no-tui` | `EPIC_NO_TUI` | off | Disable TUI, run headless |
| `--no-sandbox-warn` | `EPIC_NO_SANDBOX_WARN` | off | Suppress container/VM warning |

## TUI

The terminal interface (ratatui + crossterm) displays:
- **Task tree**: Hierarchical DFS view with status indicators (`✓` completed, `✗` failed, `▸` in progress, `·` pending)
- **Worklog**: Timestamped event stream with color-coded entries
- **Metrics panel**: Token usage per model tier, total cost, cache hit ratio, task counts (toggle with `m`)

**Keybindings:**
- `q` / `Ctrl-C`: Quit
- `t`: Toggle tail mode (auto-scroll)
- `m`: Toggle metrics panel
- `↑↓`: Scroll

## Sandboxing

Epic sandboxes the nu process via [lot](https://github.com/bitmonk8/lot) — AppContainer on Windows, namespaces + seccomp on Linux, Seatbelt on macOS. Sandbox setup is mandatory; failure returns an error (no unsandboxed fallback).

**What epic does:**
- OS-level sandboxing of nu via lot (per-phase read/write policies on the project root)
- Best-effort detection of container/VM at startup (Docker, Podman, WSL, VMware, VirtualBox, KVM, QEMU, Hyper-V)
- Warns on stderr if not running in a virtualized environment
- Tool grant bitflags restrict which tools each agent phase can use

**Additional defense:** Run epic inside a container or VM with bind-mounted project directory and restricted network access for defense-in-depth beyond the lot sandbox.

## Configuration

`epic.toml` in the project root. Generated by `epic init` or written manually.

```toml
[project]
name = "my-project"

[[verification]]
name = "build"
command = ["cargo", "build"]

[[verification]]
name = "test"
command = ["cargo", "test"]

[models]
fast = "claude-haiku-4-5-20251001"
balanced = "claude-sonnet-4-6"
strong = "claude-opus-4-6"

[limits]
max_depth = 8
retry_budget = 3
max_recovery_rounds = 2
branch_fix_rounds = 3
root_fix_rounds = 4
max_total_tasks = 100
```

All fields have defaults. No `epic.toml` is required — epic runs with sensible defaults if the file is absent.

## Module Structure

```
src/
├── main.rs                  # Entry point, CLI dispatch, TUI/headless mode
├── cli.rs                   # Clap CLI definition
├── state.rs                 # EpicState: task tree, JSON persistence, DFS ordering
├── events.rs                # Event enum (24 variants), channel types
├── init.rs                  # epic init: agent-driven project exploration
├── knowledge.rs             # ResearchTool: vault + gap-filling pipeline
├── sandbox.rs               # Container/VM detection (best-effort)
├── test_support.rs          # MockAgentService, test helpers
├── agent/
│   ├── mod.rs               # AgentService trait (10 async methods)
│   ├── reel_adapter.rs      # ReelAgent: thin adapter over reel::Agent
│   ├── wire.rs              # Wire format types, structured output schemas
│   └── prompts.rs           # Prompt assembly for all agent calls
├── orchestrator/
│   ├── mod.rs               # Coordinator: run(), execute_task(), child loops, subtask creation
│   ├── context.rs           # TreeContext: read-only tree snapshot for task methods
│   └── services.rs          # Services<A>: shared infrastructure (agent, events, vault, limits)
├── task/
│   ├── mod.rs               # Task, TaskPhase, Model, TaskOutcome, self-contained mutations
│   ├── assess.rs            # AssessmentResult
│   ├── branch.rs            # Branch decision logic and types (verify, fix, recovery, checkpoint)
│   ├── leaf.rs              # Leaf lifecycle: retry/escalation, verify, fix loop
│   ├── scope.rs             # Scope circuit breaker: git diff measurement, ScopeCheck
│   └── verify.rs            # VerificationOutcome, VerificationResult
├── config/
│   ├── mod.rs               # Config module
│   └── project.rs           # EpicConfig, ModelConfig, LimitsConfig, VerificationStep
└── tui/
    ├── mod.rs               # TuiApp: event consumer, ratatui rendering
    ├── task_tree.rs          # Task tree panel
    ├── worklog.rs            # Worklog panel
    └── metrics.rs            # Metrics panel
```

## Dependencies

| Crate | Purpose |
|-------|---------|
| `reel` | Agent session layer (tool loop, NuShell, sandbox) |
| `vault` | Document store (knowledge persistence, query, record) |
| `tokio` | Async runtime |
| `clap` | CLI argument parsing |
| `ratatui` + `crossterm` | Terminal UI |
| `serde` + `serde_json` + `toml` | Serialization |
| `anyhow` + `thiserror` | Error handling |
| `lot` | Process sandboxing (via reel) |
| `tempfile` | Atomic file writes |

## Troubleshooting

**"Goal mismatch"**: State file exists with a different goal. Delete `.epic/state.json` or use `epic resume` to continue the existing run.

**"No state file found"** on resume: No previous run to resume. Use `epic run <goal>` to start.

**Task fails repeatedly**: Leaf fix loop exhausts all model tiers (3 retries each at Haiku → Sonnet → Opus). Branch fix loop runs 3-4 rounds of fix subtasks. If all fail, recovery creates new subtasks informed by the failure. After max recovery rounds (default 2), the branch fails.

**Task limit reached**: Default cap is 100 total tasks. Increase `max_total_tasks` in `epic.toml` if the goal genuinely requires more decomposition.

**Not running in a container**: Epic warns at startup if it detects bare-metal execution. Suppress with `--no-sandbox-warn` or run inside a container/VM.
