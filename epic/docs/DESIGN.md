# Design

[README.md](../README.md) is the primary entry point — it covers architecture overview, model selection, tool access, verification config, recovery, scope circuit breaker, state persistence, event system, CLI usage, TUI keybindings, sandboxing overview, configuration examples, module structure, and dependencies. This document provides implementation depth beyond the README.

---

## Task Model

### Task State

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
    fix_attempts: Vec<Attempt>,     // Fix-specific attempts (separate from initial)
    subtask_ids: Vec<TaskId>,       // Empty for leaves
    magnitude_estimate: Option<MagnitudeEstimate>,  // Set by parent for leaves
    discoveries: Vec<String>,       // Context propagation summaries
    decomposition_rationale: Option<String>,
    depth: u32,                     // Root = 0, depth cap configurable (default 8)
    verification_fix_rounds: u32,   // Branch fix round counter
    is_fix_task: bool,              // Created by fix loop vs original decomposition
}
```

### Assessment

Single Haiku call returns `{path, model, rationale}`. Two orthogonal decisions:
- **Path**: leaf (solve directly) or branch (decompose)
- **Model**: which model executes the work

Tie-breaking bias: branch. Recovery from wrong-branch is cheaper than wrong-leaf.

If Haiku is uncertain about model, conditional escalation to Sonnet for a second assessment.

Root task is always forced to branch (guarantees recovery machinery exists). Tasks at max depth are forced to leaf.

### Leaf Path

1. Implement (model chosen by assessment)
   - Research Service available as a tool
   - Structured output via Flick `output_schema`
2. Verification gates — configurable per-project via `epic.toml`
3. File-level review — model: `max(Haiku, implementing_model)`, capped at Sonnet
4. Local simplification review — same model as file-level review
5. Fix loop on failure (see [Verification & Fix Loops](#verification--fix-loops))
6. Commit on success, full rollback on terminal failure

### Branch Path

1. Design + Decompose in same context (single level, 2-5 subtasks)
   - Research Service available as a tool
   - Decomposition strategy chosen per-problem (structural, behavioural, goal-based)
2. Create subtasks with magnitude estimates
3. Execute subtasks sequentially (DFS preorder)
4. Inter-subtask checkpoint on discoveries
5. Branch verification (Sonnet): correctness + completeness + aggregate simplification
6. Up to 3 fix rounds; root gets one additional Opus round

### Context Propagation

Two channels:
- **Task metadata** (small, injected): goal, criteria, discovery summaries
- **DocumentStore** (large, queried on demand): full research, analysis, failure records

Structural map injection per agent call:

| Scope | Content |
|---|---|
| Own task | goal, criteria |
| Parent | goal, decomposition rationale, discoveries |
| Ancestor chain | compressed one-line summaries |
| Completed siblings | goal, outcome, discovery summaries |
| Pending siblings | goal only |

### Discovery Flow

1. Agent discovers reality differs from assumptions
2. Records full detail in DocumentStore
3. Records 1-3 sentence summary in own task's `discoveries`
4. Parent runs inter-subtask checkpoint (Haiku classification):
   - **proceed**: no impact
   - **adjust**: modify pending subtasks (branch's own model)
   - **escalate**: Opus recovery (decomposition strategy invalid)
5. If discovery affects parent scope, parent records own discovery — bubbles up

### Recovery Ordering

Cheapest to most expensive:
1. Scope circuit breaker (3x magnitude estimate → immediate rollback)
2. Retry budget exhaustion → model escalation (Haiku→Sonnet→Opus)
3. Terminal leaf failure → rollback, fail to parent
4. Parent Opus recovery assessment (max `max_recovery_rounds` per branch, default 2)
5. Branch failure → escalate to grandparent
6. Global task count cap (`max_total_tasks`, default 100)

---

## Agent Layer

### Per-Phase Tool Grants

| Task Path | Phase | Tools | Purpose |
|---|---|---|---|
| Any | Assess | NONE | Pure classification call via `run_structured` (no tools) |
| Leaf | Implement | READ \| WRITE \| NU | Code changes |
| Leaf | Verify | READ | Read-only analysis |
| Branch | Design + Decompose | READ \| NU | Research + exploration, no writes |
| Branch | Verify | TASK | May spawn sub-agents for large diffs |

### Prompt Assembly

Each agent call assembles:
1. **System prompt** — role, constraints, output format
2. **Structural map** — task position in tree, sibling context
3. **Phase-specific instructions** — what this call should accomplish
4. **Tool descriptions** — available tools for this phase
5. **Verification criteria** — success conditions

Research Service is exposed as a tool during implementation and design+decompose phases.

### Structured Output

Flick returns structured JSON via `output_schema`. Epic deserializes response text into wire format types (e.g., `AssessmentWire`, `CheckpointWire`) via serde, then converts to domain types via `TryFrom`.

### Flick Integration

Epic depends on Flick as a git crate dependency. Agent calls use `FlickClient::run()` and `FlickClient::resume()` directly — no process spawning, no YAML config files, no tool result files.

```
Epic
 |-- Build flick::Config from JSON in-memory
 |-- resolve_provider(&config).await
 |-- FlickClient::new(config, provider)
 |-- client.run(query, &mut context).await
 |
 |   (if ToolCallsPending)
 |-- Execute tools locally
 |-- Build Vec<ContentBlock::ToolResult>
 |-- client.resume(&mut context, tool_results).await
 |   (repeat until Complete)
 |
 |-- Extract text from FlickResult.content
```

#### Key Types

| Epic usage | Flick type |
|---|---|
| Config construction | `flick::Config` (parsed from JSON via `Config::from_str`) |
| Client | `flick::FlickClient` |
| Conversation state | `flick::Context` |
| Response | `flick::FlickResult` |
| Content blocks | `flick::ContentBlock` (Text, Thinking, ToolUse, ToolResult) |
| Result status | `flick::result::ResultStatus` (Complete, ToolCallsPending, Error) |
| Provider resolution | `flick::resolve_provider()` |
| Errors | `flick::FlickError` |

#### Timeout Handling

`tokio::time::timeout` wraps `client.run()` and `client.resume()`. On timeout, the HTTP request inside Flick is dropped (reqwest future cancelled).

#### Credential Management

Flick's `CredentialStore` (default `~/.flick/`) resolves API keys. Credential name passed via `--credential` CLI option (default: `anthropic`).

---

## Document Store

### Storage

Centralized knowledge at `.epic/docs/`. All tasks see all accumulated knowledge, organized by topic. File-based (markdown). Small document counts make this sufficient; SQLite index can layer on later.

### Core Documents

| Document | Purpose |
|---|---|
| EPIC.md | Project overview + document index |
| REQUIREMENTS.md | Captured from interactive session |
| CHANGELOG.md | Append-only mutation log |
| FINDINGS.md | Accumulated discoveries |
| DESIGN.md | Design decisions |
| Topic-specific | Created as needed by librarian |

### Operations

1. **Bootstrap** (pre-TUI): Convert requirements into initial document set
2. **Query**: Search documents for relevant knowledge. Returns extract + source references
3. **Record**: Write findings. Librarian decides file placement, merging, restructuring

### Librarian

A Flick agent (Haiku, read-only tools) manages document placement, merging, and restructuring. Prevents unbounded growth. Handles deduplication.

### Research Service

Exposed as a tool to calling agents:

```
research_query(question, scope) -> ResearchResult {
    answer: String,
    document_refs: Vec<String>,  // "FILENAME > Section" format
    gaps_filled: u32,
}
```

Scope: PROJECT (codebase exploration), WEB (web search), or BOTH.

Workflow:
1. Check DocumentStore for existing knowledge
2. Identify gaps
3. Fill gaps via codebase exploration or web search (Haiku)
4. Store results in DocumentStore
5. Return structured answer with provenance

Demand-driven — called when an agent hits uncertainty, not as a mandatory preamble.

---

## Verification & Fix Loops

### Verification Types

```rust
struct VerificationStep {
    name: String,           // "Build", "Lint", "Test"
    command: Vec<String>,   // e.g., ["cargo", "build"]
    timeout: u32,
}
```

### Error Handling

- Error deduplication: group by pattern, max 3 per group, 50 lines total
- Regression detection: did fix introduce new failures?
- Build check is fail-fast (run before other verification)

### Leaf Fix Loop

When leaf verification fails:

1. Record verification failure reason
2. Check scope circuit breaker. If tripped, fail with `SCOPE_EXCEEDED`
3. Call `fix_leaf()` — agent re-executes with failure context
4. Re-verify
5. On pass: complete
6. On fail: increment `fix_retries_at_tier`
   - `< 3`: loop to step 2
   - `== 3`: escalate to next model tier, reset counter, loop to step 2
   - Opus exhausted: terminal failure

Starting tier is the model that produced the failing output (if Sonnet wrote it, Haiku won't fix it).

```rust
async fn fix_leaf(
    &self,
    ctx: &TaskContext,
    model: Model,
    failure_reason: &str,
    attempt: u32,
) -> Result<LeafResult>;
```

Prompt includes original goal, verification failure reason, attempt number, and instructions to fix specific issues rather than rewrite.

On resume: if task is in `Verifying` phase with `fix_attempts.len() > 0`, the fix loop resumes with the correct counter.

### Branch Fix Loop

When branch verification fails:

1. Increment `verification_fix_rounds`
2. Check round budget:
   - Non-root: max 3 rounds (Sonnet)
   - Root: max 4 rounds (3 Sonnet + 1 Opus)
3. Call `design_fix_subtasks()` — agent analyzes issues, produces fix subtask specs
4. Create and execute fix subtasks through the normal pipeline (assess → execute → verify)
5. Re-verify branch
6. On pass: complete. On fail: loop or terminal failure

```rust
async fn design_fix_subtasks(
    &self,
    ctx: &TaskContext,
    model: Model,
    verification_issues: &str,
    round: u32,
) -> Result<DecompositionResult>;
```

Returns `DecompositionResult` (reuses existing type). Fix subtask specs are structurally identical to normal decomposition output.

#### Branch Verification Content

Each round performs three reviews:
- **Correctness**: do changes satisfy the goal?
- **Completeness**: are all aspects addressed?
- **Aggregate simplification**: can the combined output be simplified?

#### Fix Subtask Rules

- Go through the full pipeline: assess → execute → verify
- CAN use the leaf fix loop if their own verification fails
- CANNOT trigger the branch fix loop (prevents recursive fix chains)
- Receive context about the original branch goal and what they're fixing
- Recovery subtasks inherit parent's `recovery_rounds` counter to prevent exponential cost growth
- Before creating any subtasks, orchestrator checks `max_total_tasks` cap

### Scope Circuit Breaker

Before each fix attempt (leaf or branch), measure actual change magnitude:

1. Parent's `assess()` returns magnitude estimates with 50% conservative buffer
2. Run `git diff --numstat` against the task's workspace
3. If any metric exceeds 3x the estimate: fail with `SCOPE_EXCEEDED`, roll back

```rust
async fn check_scope_circuit_breaker(
    &self,
    task_id: TaskId,
    workspace: &Path,
) -> Result<ScopeCheck>;

enum ScopeCheck {
    WithinBounds,
    Exceeded { metric: String, actual: u64, limit: u64 },
}

pub struct Magnitude {
    pub max_lines_added: u64,
    pub max_lines_modified: u64,
    pub max_lines_deleted: u64,
}
```

Not checked on initial execution — only on fix retries. Skipped if no magnitude estimate exists.

### Events

```rust
FixAttempt { task_id: TaskId, attempt: u32, model: Model }
FixModelEscalated { task_id: TaskId, from: Model, to: Model }
BranchFixRound { task_id: TaskId, round: u32, model: Model }
FixSubtasksCreated { task_id: TaskId, count: usize, round: u32 }
```

---

## Sandboxing

### Security Isolation (User-Managed)

Epic does not implement OS-level security sandboxing. The only robust boundary is a user-managed VM or container.

#### Detection Signals

| Environment | Signal |
|---|---|
| Docker | `/.dockerenv` exists, or `/proc/1/cgroup` contains `docker`/`containerd` |
| Podman | `/.dockerenv` or `/run/.containerenv` exists |
| systemd-nspawn | `systemd-detect-virt` returns `systemd-nspawn` |
| Linux VM | `systemd-detect-virt` returns a hypervisor name |
| macOS VM | `sysctl kern.hv_vmm_present` returns 1 |
| WSL | `/proc/version` contains `Microsoft` or `WSL` |
| Windows VM | PowerShell `(Get-CimInstance Win32_ComputerSystem).Model` contains VM vendor string |
| Windows container | No reliable detection signal |

Detection is best-effort. False positive = unnecessary warning (acceptable). Epic will not refuse to run outside a container.

### Operational Correctness (lot)

The nu tool spawns a persistent `nu --mcp` process inside a [lot](https://github.com/bitmonk8/lot) sandbox. Per-phase policies control access:

| Phase | Project Root | Temp Dirs | Network |
|---|---|---|---|
| Assess / Decompose / Verify | Read-only | Writable | Allowed |
| Execute / Fix | Writable | Writable | Allowed |

Platform mechanisms:
- **Linux**: namespaces + seccomp-BPF + rlimit
- **macOS**: Seatbelt (`sandbox_init`) + rlimit
- **Windows**: AppContainer + Job objects

Sandbox is mandatory: if setup fails, the tool call returns an error. No unsandboxed fallback.

### Enforcement Layers

Three layers, all retained:
1. **`ToolGrant` bitflags** — controls which tools are offered per phase
2. **`safe_path()` containment** — validates paths in epic's tool implementations
3. **lot sandbox** — OS-level process isolation on the nu process

---

## NuShell Runtime

### MCP Server

NuShell has built-in MCP server support (default since v0.110.0). Epic uses stdio transport (`nu --mcp`).

#### MCP Tools Exposed

- **evaluate** — execute arbitrary NuShell commands/pipelines (primary tool)
- **find_command / list_command** — discover available NuShell commands

#### Structured Responses

```json
{ "history_index": 5, "cwd": "/home/user/project", "output": "..." }
```

ANSI coloring disabled. Rich error diagnostics with line/column details.

### MCP Client

**Protocol**: JSON-RPC 2.0 over stdio. Epic writes requests to nu's stdin, reads responses from stdout.

**Tool dispatch**: `tool_nu` sends `tools/call` with tool name `"evaluate"` and command string as argument.

### Session Lifecycle

One nu MCP process per agent session. Each session has a fixed phase → fixed sandbox policy. The nu process is spawned lazily at first tool call, killed when session returns structured output.

Sessions are oneshot — once returned, never reused. A new session gets a fresh nu process. This means:
- No phase-change restart logic needed
- Multiple tool calls within a session share the nu process state (env vars, cwd, variables)
- Sandbox correctness guaranteed by construction

### Timeout Handling

On timeout, epic kills the nu MCP process and returns an error. Next tool call spawns a fresh session (state lost). Agent recovers from the error message. No MCP-level request cancellation needed.

### Compatibility

| Aspect | Detail |
|---|---|
| Exit codes | Standard (0 = success), included in MCP response |
| Environment | NuShell reads env vars; lot's `forward_common_env()` handles filtering |
| Signals | Responds to SIGKILL / TerminateProcess normally |
| Syntax | LLM agents generate NuShell syntax; tool name/description handles this |

### Implementation Map

| Component | Location |
|---|---|
| Binary | `build.rs` + `target/nu-cache/` |
| MCP client | `src/agent/nu_session.rs` (`NuSession`) |
| Tool layer | `src/agent/tools.rs` (`tool_nu` → `NuSession::evaluate`) |
| Sandbox policy | `src/agent/nu_session.rs` (`build_nu_sandbox_policy`) |
| Flick integration | `src/agent/flick.rs` (`ToolExecutor::execute` takes `&NuSession`) |

---

## TUI

### Layout

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

### Worklog Content

Streams for the current task:
- Phase start/end with duration
- Agent text output (event-level, no token streaming)
- Tool calls (summarized)
- Verification results (pass/fail per step)
- Discovery notifications
- Error/fix loop iterations

### Metrics Panel

Token usage per model tier, session cost, task count (completed/total). Toggle with `m`.

### Event System

Full event list consumed by TUI:

```rust
TaskRegistered { task_id, parent_id, goal, depth }
PhaseTransition { task_id, phase }
PathSelected { task_id, path }
ModelSelected { task_id, model }
ModelEscalated { task_id, from, to }
SubtasksCreated { parent_id, child_ids }
TaskCompleted { task_id, outcome }
RetryAttempt { task_id, attempt, model }
DiscoveriesRecorded { task_id, count }
CheckpointAdjust { task_id }
CheckpointEscalate { task_id }
FixAttempt { task_id, attempt, model }
FixModelEscalated { task_id, from, to }
BranchFixRound { task_id, round, model }
FixSubtasksCreated { task_id, count, round }
RecoveryStarted { task_id, round }
RecoveryPlanSelected { task_id, approach }
RecoverySubtasksCreated { task_id, count, round }
TaskLimitReached { task_id }
```

Events also feed structured JSONL file logging for post-run analysis.

---

## Configuration

### Resolution Order

Project config (highest priority wins):
1. `epic.toml` in current directory
2. `.epic/config.toml` in current directory
3. Defaults (no verification steps — warn and proceed)

User-level defaults (`~/.config/epic/config.toml`) are loaded first, overridden field-by-field by project config.

### Verification Steps

```toml
[[verification]]
name = "Build"
command = ["cargo", "build"]
timeout = 300

[[verification]]
name = "Lint"
command = ["cargo", "clippy", "--", "-D", "warnings"]
timeout = 300

[[verification]]
name = "Test"
command = ["cargo", "test"]
timeout = 600
```

Python example:
```toml
[[verification]]
name = "Build"
command = ["python", "please.py", "build"]
timeout = 300
```

### Model Preferences

```toml
[models]
fast = "claude-haiku-4-5-20251001"   # Assessment, checkpoints, document ops
balanced = "claude-sonnet-4-6"      # Implementation, branch verification
strong = "claude-opus-4-6"          # Recovery, complex decomposition
```

### Init Flow

1. **Explore** — Agent scans for build system markers:
   - Build systems: `Cargo.toml`, `package.json`, `pyproject.toml`, `Makefile`, `CMakeLists.txt`, `build.gradle`, `go.mod`
   - Test frameworks: test directories, config files (`jest.config`, `pytest.ini`)
   - Linters/formatters: `clippy`, `eslint`, `ruff`, `black`, `prettier`, `golangci-lint`
   - CI config: `.github/workflows/`, `.gitlab-ci.yml` — extract commands as hints

2. **Present findings** — Report detected tooling

3. **Confirm interactively** — User accepts, modifies, adds, or declines each step; model/limit preferences

4. **Write `epic.toml`** — Declined options included as comments

Fallback: no markers detected → minimal config with empty verification and commented examples.

---

## Dependency Injection

All major components receive dependencies explicitly. No globals, statics, or singletons.

Key dependency types:
- `TaskContext` and `FlickAgent` — bundle Flick config, document store, verification config. Each agent call creates a new `FlickClient` (stateless per-call)
- `EventEmitter` — trait object for logging/TUI events
- `ProjectConfig` — verification steps, paths, model preferences (loaded from TOML)
- `EpicState` — task tree and session state (owned by orchestrator)
