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
    current_model: Option<Model>,   // Selected by assessment, changes on escalation
    attempts: Vec<Attempt>,         // Retry/escalation history
    fix_attempts: Vec<Attempt>,     // Fix-specific attempts (separate from initial)
    subtask_ids: Vec<TaskId>,       // Empty for leaves
    magnitude_estimate: Option<MagnitudeEstimate>,  // Set by parent for leaves
    magnitude: Option<Magnitude>,   // Concrete line-count bounds from assessment
    discoveries: Vec<String>,       // Context propagation summaries
    checkpoint_guidance: Option<String>,  // Accumulated checkpoint guidance from parent
    decomposition_rationale: Option<String>,
    depth: u32,                     // Root = 0, depth cap configurable (default 8)
    verification_fix_rounds: u32,   // Branch fix round counter
    is_fix_task: bool,              // Created by fix loop vs original decomposition
    recovery_rounds: u32,           // Branch recovery round counter
    usage: TaskUsage,               // Accumulated token/cost usage
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
   - Structured output via reel `output_schema`
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

## Orchestrator Architecture

### Coordinator / Task Split

The orchestrator is a pure coordinator. Tasks own their behavior.

- **Leaf tasks** execute their full lifecycle internally via `Task::execute_leaf()`: agent calls, retry/escalation (Haiku -> Sonnet -> Opus), verification gates, file-level review, fix loop, scope circuit breaker. The orchestrator calls `run_leaf()` which builds a `TreeContext`, gets `&mut Task`, calls `execute_leaf`, and handles completion/failure.

- **Branch tasks** own decision logic via Task methods in `task/branch.rs`: `verify_branch()`, `fix_round_budget_check()`, `design_fix()`, `recovery_budget_check()`, `assess_and_design_recovery()`, `handle_checkpoint()`, `check_branch_scope()`. Each method receives `TreeContext` + `Services`, makes agent calls, mutates only `self`, and returns a structured decision enum (`BranchVerifyOutcome`, `FixBudgetCheck`, `RecoveryDecision`, `ChildResponse`). The orchestrator acts on these decisions by performing cross-task operations: creating subtasks, executing children, failing pending siblings.

- **`Services<A>`** bundles shared infrastructure (agent, events, vault, limits, paths) passed to task methods during execution.

- **`TreeContext`** is a read-only owned snapshot of tree state (parent goal, siblings, children, checkpoint guidance) built by the orchestrator before calling task methods. Resolves `&state` / `&mut task` borrow conflicts. Rebuilt before each Task method call when task state may have changed.

### Task Self-Contained Mutations

`Task` has mutation methods for operations that only touch the task itself: `set_assessment`, `record_attempt`, `record_discoveries`, `set_model`, `set_decomposition_rationale`, `set_checkpoint_guidance`, `append_checkpoint_guidance`, `increment_fix_rounds`, `increment_recovery_rounds`, `accumulate_usage`, `trailing_attempts_at_tier`. The orchestrator calls these instead of directly mutating task fields.

### File Structure

```
orchestrator/
  mod.rs          Thin coordinator: run(), execute_task(), run_leaf(),
                  execute_branch() (decomposition + child loop),
                  finalize_branch() (delegates to Task::verify_branch),
                  branch_fix_loop() (delegates to Task fix/design methods),
                  attempt_recovery() (delegates to Task recovery methods),
                  create_subtasks(), fail_pending_children()
  context.rs      TreeContext struct, build_tree_context(), build_context()
  services.rs     Services<A> struct
task/
  mod.rs          Task struct, types, self-contained mutation methods
  leaf.rs         Task::execute_leaf(): full lifecycle (execute -> verify -> fix loop)
  branch.rs       Branch decision types (SubtaskSpec, DecompositionResult,
                  CheckpointDecision, BranchVerifyOutcome, FixBudgetCheck,
                  RecoveryDecision, ChildResponse) and
                  Task methods (verify_branch, fix_round_budget_check,
                  design_fix, recovery_budget_check, assess_and_design_recovery,
                  handle_checkpoint, check_branch_scope)
  scope.rs        Scope circuit breaker: git_diff_numstat, evaluate_scope, ScopeCheck
  assess.rs       AssessmentResult
  verify.rs       VerificationOutcome, VerificationResult, VerifyOutcome
```

---

## Agent Layer

### Per-Phase Tool Grants

| Task Path | Phase | Grants | Purpose |
|---|---|---|---|
| Any | Assess | NONE | Pure classification call via `run_structured` (no tools) |
| Leaf | Implement | WRITE \| NU | Code changes |
| Leaf | Verify | NU | Read-only analysis via nu |
| Branch | Design + Decompose | NU | Research + exploration, no writes |
| Branch | Verify | NU | Verification via nu |

### Prompt Assembly

Each agent call assembles:
1. **System prompt** — role, constraints, output format
2. **Structural map** — task position in tree, sibling context
3. **Phase-specific instructions** — what this call should accomplish
4. **Tool descriptions** — available tools for this phase
5. **Verification criteria** — success conditions

Research Service is exposed as a tool during implementation and design+decompose phases.

### Structured Output

Reel returns structured JSON via `output_schema`. Epic deserializes response text into wire format types (e.g., `AssessmentWire`, `CheckpointWire`) via serde, then converts to domain types via `TryFrom`.

### Reel Integration

Epic delegates agent execution to reel. No direct Flick calls — reel owns the conversation turn loop, tool dispatch, and NuShell runtime.

**Startup**: Epic builds a `reel::AgentEnvironment` once, containing the model registry (built from `ModelConfig` + credential name), provider registry (loaded from flick's config directory), project root, and call timeout. A `reel::Agent` is constructed from this environment.

**Per agent call**: Epic builds a `reel::AgentRequestConfig` containing a `reel::RequestConfig` (model key, system prompt, output schema), a `reel::ToolGrant` (phase-dependent), and custom tools (`ResearchQuery` vault tool when vault is enabled, otherwise empty). Then calls `reel::Agent::run()`.

**Return**: Reel returns `reel::RunResult<T>` with the parsed output, token usage, tool call count, and response hash.

#### Key Types

| Epic usage | Reel type |
|---|---|
| Agent entry point | `reel::Agent` |
| Startup environment | `reel::AgentEnvironment` |
| Per-call config | `reel::AgentRequestConfig` |
| Run result | `reel::RunResult<T>` |
| Phase tool access | `reel::ToolGrant` |
| Request parameters | `reel::RequestConfig` |
| Model registry | `reel::ModelRegistry` |
| Provider registry | `reel::ProviderRegistry` |

---

## Document Store

> **Implementation**: The vault sibling crate (`../vault/vault`) implements the document store. See vault's own `docs/DESIGN.md` for detailed internals.

### Storage

Centralized knowledge at `.epic/docs/`. All tasks see all accumulated knowledge, organized by topic. File-based (markdown). Small document counts make this sufficient; SQLite index can layer on later.

Vault's storage structure:
- `raw/` — client-provided content (versioned: `NAME_1.md`, `NAME_2.md`, ...)
- `derived/` — librarian-produced documents (merged, restructured by librarian)
- `CHANGELOG.md` — append-only JSONL mutation log

### Operations

1. **Bootstrap** (pre-orchestrator): Convert goal text into initial document set (`PROJECT.md`, `REQUIREMENTS.md`)
2. **Query**: Read-only search against derived documents. Returns `QueryResult { coverage, answer, extracts }`
3. **Record**: Write findings to raw, librarian integrates into derived. Used for discoveries, verification failures, checkpoint decisions, recovery strategies
4. **Reorganize**: Full sweep of derived documents — merge, split, deduplicate. Called after root branch children complete

### Librarian

A reel agent manages document placement, merging, and restructuring. Vault constructs its own `reel::Agent` with `project_root` set to the vault storage root (separate from epic's project root). Per-operation model selection via `VaultConfig`.

### Research Service

Exposed as a `ResearchQuery` custom tool via reel's `ToolHandler` trait. Injected into `AgentRequestConfig::custom_tools` for execute, decompose, fix, and recovery design phases.

```
ResearchQuery { question: String, scope?: "vault" | "project" }
  -> formatted text (gaps_filled + answer + document_refs)
```

Implements a multi-step gap-filling pipeline:

```
research_query(question, scope)
    |-- Step 1: vault.query(question) -> QueryResult with coverage
    |-- Step 2: If coverage != Full -> gap identification (Haiku, no tools, structured output)
    |           Returns list of specific gaps
    |-- Step 3: If gaps exist and scope == Project:
    |   |-- For each gap:
    |       |-- Spawn Haiku agent with read-only tools to explore codebase
    |       |-- vault.record() findings (best-effort)
    |-- Step 4: Synthesis (Haiku, no tools) -- combine vault knowledge + exploration findings
    |-- Return ResearchResult { answer, document_refs, gaps_filled }
```

Short-circuit paths:
- Full coverage from vault: return vault answer immediately (no gap analysis)
- Vault-only scope: return vault answer immediately (no exploration)
- Gap analysis says sufficient: return vault answer (no exploration)

All internal agent calls use Haiku ("fast" model key). Exploration agents get `ToolGrant::TOOLS` (read-only: Read, Glob, Grep, NuShell). Each pipeline step's usage is tracked in the usage sink and folded into the calling task's cost.

**Scope parameter**: `vault` queries stored knowledge only. `project` (default) queries vault then explores the codebase to fill gaps. WEB scope (web search) is deferred.

**Error handling**: Each pipeline step degrades gracefully. Gap identification failure falls back to vault answer. Exploration failure for a gap is logged and skipped. Synthesis failure falls back to vault answer with raw findings appended. Vault record operations are best-effort.

Demand-driven — called when an agent hits uncertainty, not as a mandatory preamble.

### Integration Points

| Site | Operation | Document |
|---|---|---|
| Leaf produces discoveries | `record` | `FINDINGS` |
| Verification fails | `record` | `VERIFICATION_FAILURE` |
| Checkpoint adjust | `record` | `FINDINGS` |
| Recovery assessment | `record` | `FINDINGS` |
| Root branch children complete | `reorganize` | — |
| Bootstrap (new run) | `bootstrap` | — |

All vault operations are best-effort. Failures are logged but never abort the orchestrator run.

### Usage Tracking

Vault returns `SessionMetadata` from every operation. Converted to epic's `SessionMeta` via `SessionMeta::from_vault()` and accumulated on the task that triggered the operation. Vault query costs from the `ResearchQuery` tool are captured via an `Arc<Mutex<Vec<SessionMeta>>>` sink drained after each agent run, then folded into the calling task's usage.

### Configuration

```toml
[vault]
enabled = true              # default false
storage = ".epic/docs"      # relative to project root
bootstrap_model = "balanced"
query_model = "fast"
record_model = "fast"
reorganize_model = "balanced"
```

Model names are reel registry keys resolved against epic's `ModelConfig`.

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
// Task method — leaf uses check_scope(), branch uses check_branch_scope()
async fn check_branch_scope<A: AgentService>(
    &self,
    svc: &Services<A>,
) -> ScopeCheck;

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

### Security Isolation

Epic sandboxes the nu process via lot (see [Operational Correctness](#operational-correctness-lot) below). A user-managed VM or container provides an additional defense-in-depth layer.

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

Platform mechanisms detailed in [Platform Sandbox Capabilities](#platform-sandbox-capabilities) below.

Sandbox is mandatory: if setup fails, the tool call returns an error. No unsandboxed fallback.

### Enforcement Layers

Two layers:
1. **`ToolGrant` bitflags** — phase marker controlling which tool definitions are offered to the agent (WRITE, NU). Does not enforce access — prevents agents from wasting tokens on tool calls that will fail.
2. **lot sandbox** — OS-level process isolation on the nu process. Sole access control mechanism. All file I/O routes through the sandboxed nu process.

### Platform Sandbox Capabilities

| Capability | Linux | macOS | Windows |
|---|---|---|---|
| Read-only enforcement | Mount NS (`MS_RDONLY` remount) | Seatbelt SBPL (`file-read*` only) | AppContainer ACLs (`FILE_GENERIC_READ`) |
| Read-write enforcement | Mount NS (no `MS_RDONLY`) | Seatbelt (`file-read*` + `file-write*`) | ACLs (`FILE_GENERIC_READ \| WRITE`) |
| Executable control | Mount flags (`MS_NOEXEC`) | Seatbelt (`process-exec`, `file-map-executable`) | ACLs (`FILE_GENERIC_EXECUTE`) |
| Path hiding | Full (only mounted paths exist) | Default-deny (access denied) | ACL-based (access denied) |
| Always available? | Requires unprivileged user NS | Always | Always |

**Path hiding difference**: Linux mount namespaces eliminate paths entirely; macOS/Windows deny access. Equivalent for epic's use case.

**Linux caveat**: Unprivileged user namespaces may be disabled by kernel config or AppArmor.

### Windows: AppContainer Prerequisites

AppContainer sandboxes require two prerequisites for nu commands to work correctly:

1. **NUL device ACL** — AppContainer blocks access to `\\.\NUL`. Nu's MCP mode sets `stdin(Stdio::null())` for external commands, which opens `\\.\NUL` — causing `rg` spawns to fail with `ERROR_ACCESS_DENIED`.
2. **Ancestor traverse ACEs** — Nu built-in commands (`open`, `ls <file>`, `mkdir`) route through `nu_glob`, which calls `fs::metadata()` on each ancestor directory. Without `FILE_TRAVERSE | SYNCHRONIZE` ACEs for `ALL APPLICATION PACKAGES` on ancestors, these calls fail with `ACCESS_DENIED`.

**Fix**: `epic setup` (one-time, elevated) calls `lot::grant_appcontainer_prerequisites(&[project_root])`, which grants both NUL device access and ancestor traverse ACEs. `epic run` / `epic resume` check `lot::appcontainer_prerequisites_met(&[project_root])` at startup and fail early if not configured.

**lot API** (Windows-only, exported from crate root):

| Function | Signature | Behavior |
|---|---|---|
| `appcontainer_prerequisites_met(paths)` | `(&[&Path]) → bool` | Checks NUL device DACL and ancestor traverse ACEs for all paths |
| `is_elevated()` | `→ bool` | Queries `TOKEN_ELEVATION` on current process token |
| `grant_appcontainer_prerequisites(paths)` | `(&[&Path]) → lot::Result<()>` | Idempotent — grants NUL device access and `FILE_TRAVERSE \| SYNCHRONIZE` ACEs on ancestors up to volume root |

---

## Unified Tool Layer

> The tool layer lives in the reel crate. This section documents reel's design for reference.

### Design Rationale

**TOCTOU elimination**: Prior to the unified tool layer, epic enforced filesystem boundaries two ways: `safe_path()` in the Rust process (path canonicalization, symlink guards) and lot sandbox on the nu process. This created TOCTOU race conditions because epic's process was unsandboxed. Moving all file operations into the sandboxed nu process eliminates the race class by construction — lot enforces boundaries at the syscall level.

**Claude Code alignment**: Claude models are trained on Claude Code's tool interface (Read, Write, Edit, Glob, Grep, Bash). Aligning epic's tool schemas with this interface reduces tool-use errors. Epic's shell tool is named `NuShell` (not `Bash`) to steer models toward NuShell syntax.

### Tool Schemas

Agents see six tools with JSON parameter schemas. Epic translates each JSON tool call into a nu command via `translate_tool_call()` and formats the nu response back via `format_tool_result()`. Agents never write nu syntax for file operations.

| Tool | Parameters | Required |
|---|---|---|
| Read | `file_path`, `offset` (int, 1-based), `limit` (int) | `file_path` |
| Write | `file_path`, `content` | both |
| Edit | `file_path`, `old_string`, `new_string`, `replace_all` (bool) | first three |
| Glob | `pattern`, `path` | `pattern` |
| Grep | `pattern`, `path`, `output_mode`, `glob`, `include_type`, `case_insensitive`, `line_numbers`, `context_after`, `context_before`, `context`, `multiline`, `head_limit` | `pattern` |
| NuShell | `command`, `description`, `timeout` (int, default 120, max 600) | `command` |

**Deliberate divergences from Claude Code**: `file_path` accepts project-relative paths (CC requires absolute). Read rejects files over 256 KiB; output is truncated at 64 KiB (`MAX_NU_OUTPUT`). Grep uses `snake_case` parameter names (`case_insensitive`, `context_after`) instead of flag-style (`-i`, `-A`). Grep's `type` parameter renamed to `include_type` to avoid JSON Schema keyword collision. NuShell has no `run_in_background` (sessions are single-threaded). NuShell's `description` parameter is accepted for logging/TUI display but does not affect execution.

### Bidirectional Translation

Two conversion points, both in reel's `tools.rs`:

**Inbound** (`translate_tool_call`): JSON tool parameters → quoted nu command string → `nu_session.evaluate()`. String parameters escaped via `quote_nu()` (single-quote wrapping with escaping for special characters). Example: `Read {file_path: "src/main.rs", offset: 10}` → `reel read 'src/main.rs' --offset 10`.

**Outbound** (`format_tool_result`): Nu NUON response → Claude-formatted text:

| Tool | Nu returns | Rust formats as |
|---|---|---|
| Read | `{path, size, total_lines, offset, lines_returned, lines: [{line, text}, ...]}` | Line-numbered content (cat -n style), total lines as context |
| Write | `{path, bytes_written}` | Confirmation message |
| Edit | `{path, replacements}` | Confirmation message with count |
| Glob | `list<string>` | Newline-separated file paths |
| Grep | `{exit_code, output}` | Pass-through (rg output is Claude-compatible). Exit code 1 (no matches) is not an error. |
| NuShell | varies | Pass-through (raw MCP output) |

### Error Mapping

Nu `error make` messages surface as JSON-RPC errors (code `-32603`). Epic's MCP response parser converts these to `NuOutput { is_error: true }`, which becomes an `isError: true` tool result visible to the agent. The session remains alive after errors. Sandbox permission errors (lot denying access) produce OS-level errors surfaced similarly.

The MCP `evaluate` tool uses `input` as its parameter name.

### Nu Custom Commands

Commands defined in `reel_config.nu`, loaded via `nu --mcp --config <path> --env-config <path>`. Use nu subcommand syntax (`def "reel read" [...]`) — `help reel` lists all subcommands. Commands use nu-native types internally and return structured records/lists. The Rust translation layer formats structured output for Claude.

**Config injection rationale**: `--commands` + `--mcp` does not work — custom commands defined via `--commands` are not visible to subsequent `evaluate` calls (separate scope). `--config` overrides default config resolution, preventing user config from loading (reproducibility, sandbox-leakage prevention). Config files placed alongside nu binary in `target/nu-cache/` — lot already grants access to the binary's directory.

**Platform note**: Config file paths must be absolute. On Windows, forward-slash paths work (`C:/path/config.nu`). Unix-style paths (`/tmp/...`) do not resolve on Windows.

**Key implementation details**:
- `reel read`: 256 KiB size cap via `error make`. Returns structured record with line-numbered content.
- `reel write`: 1 MiB size cap. Creates parent directories via `mkdir`.
- `reel edit`: `split row` and `str replace` are literal (not regex — nu default since 0.84.0). Uniqueness enforced unless `--replace-all`.
- `reel glob`: 1000 result cap.
- `reel grep`: Wraps `rg` via `^rg ...$args | complete`. Uses `--color=never` to prevent ANSI codes.
- Nu `filesize` type: `into filesize` converts `int` → `filesize` for cross-type comparisons (e.g., `bytes length | into filesize > 1MiB`).

### Phase → Lot Policy → Tool Set

| Phase | Lot Policy | Tools Offered |
|---|---|---|
| Analyze (verify, file-review) | `read_path(project_root)` | Read, Glob, Grep, NuShell |
| Execute (leaf, fix-leaf) | `write_path(project_root)` | Read, Write, Edit, Glob, Grep, NuShell |
| Decompose (design, recovery-design) | `read_path(project_root)` | Read, Glob, Grep, NuShell |
| Assess / Checkpoint | N/A (no nu process) | None |

Security does not depend on tool filtering — lot enforces access regardless.

---

## NuShell Runtime

> The NuShell runtime lives in the reel crate. This section documents reel's design for reference.

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

One nu MCP process per agent session. Each session has a fixed phase → fixed sandbox policy. The nu process is spawned eagerly at session creation for any agent call that has tool grants, and killed when the session returns structured output.

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
| Binary | reel `build.rs` + `target/nu-cache/` |
| MCP client | reel `src/nu_session.rs` (`NuSession`) |
| Tool layer | reel `src/tools.rs` (`tool_nu` → `NuSession::evaluate`) |
| Sandbox policy | reel `src/nu_session.rs` (`build_nu_sandbox_policy`) |
| Agent adapter | `src/agent/reel_adapter.rs` (`ReelAgent` → `reel::Agent`) |

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
FileLevelReviewCompleted { task_id, passed }
RecoveryStarted { task_id, round }
RecoveryPlanSelected { task_id, approach }
RecoverySubtasksCreated { task_id, count, round }
TaskLimitReached { task_id }
UsageUpdated { task_id, phase_cost_usd, total_cost_usd }
VaultBootstrapCompleted { cost_usd }
VaultRecorded { task_id, document }
VaultReorganizeCompleted { merged, restructured, deleted }
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
- `TaskContext` and `ReelAgent` — bundle reel config, document store, verification config. Each agent call builds a `reel::AgentRequestConfig` and calls `reel::Agent::run()`
- `EventEmitter` — trait object for logging/TUI events
- `ProjectConfig` — verification steps, paths, model preferences (loaded from TOML)
- `EpicState` — task tree and session state (owned by orchestrator)
