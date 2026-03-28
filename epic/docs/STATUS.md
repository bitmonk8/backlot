# Project Status

## Current Phase

**Core orchestration, knowledge layer, and file-level review implemented.**

## What Is Implemented

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `ReelAgent` implementing `AgentService` (10 methods) — thin adapter building `reel::AgentRequestConfig` per phase, delegates tool loop and tool execution to reel crate
- **Reel crate** (git rev `93f35ef`) — standalone agent session layer extracted from epic. Contains: `Agent` runtime (tool loop with resume), 6 built-in tools (`Read`, `Write`, `Edit`, `Glob`, `Grep`, `NuShell`), `NuSession` (persistent `nu --mcp` process inside lot sandbox), `ToolHandler` trait for custom tool dispatch, `ToolGrant` bitflags (WRITE/TOOLS/NETWORK), `ModelRegistry`/`ProviderRegistry` re-exports from flick. Nu config — `reel_config.nu` and `reel_env.nu` written to `target/nu-cache/` by `build.rs`, custom commands (`reel read`, `reel write`, `reel edit`, `reel glob`, `reel grep`). `REEL_RG_PATH` env var for rg binary injection. `RunResult` exposes `Usage` (tokens + cost), `TurnRecord` transcript, and per-call API latency.
- State persistence via `.epic/state.json` — atomic writes, resume, goal mismatch detection, corrupt state handling, cycle-safe DFS
- TUI via ratatui + crossterm — task tree, worklog, metrics panels, keyboard controls
- CLI via clap — `init`, `run <goal>`, `resume`, `status`, `setup` subcommands
- `epic init` — agent-driven interactive configuration scaffolding
- Container/VM startup detection with suppressible warning
- Process sandboxing delegated to reel (which uses lot) — nu tool runs inside a persistent `nu --mcp` process spawned inside an OS-native sandbox (AppContainer on Windows, namespaces+seccomp on Linux, Seatbelt on macOS); one nu MCP session per agent call, sandbox is mandatory (no unsandboxed fallback). `epic setup` grants AppContainer prerequisites via `lot::grant_appcontainer_prerequisites()`; `run`/`resume` check `lot::appcontainer_prerequisites_met(&[project_root])` and fail early if not configured.
- Context propagation — `TaskContext` carries discoveries, parent goals, sibling summaries, checkpoint guidance. Structural map injection in prompts (ancestor chain, completed/pending siblings).
- Discovery flow — in-memory tracking via `task.discoveries`. Inter-subtask checkpoint with Haiku classification (proceed/adjust/escalate). Discovery bubbling to parent.
- Assessment — Haiku call returns path (leaf/branch) + model selection. Root forced to branch, max-depth forced to leaf.
- Verification & fix loops — leaf fix loop with model escalation (Haiku→Sonnet→Opus, 3 retries per tier), branch fix loop (3 Sonnet rounds + 1 Opus round for root), scope circuit breaker (3x magnitude estimate via `git diff --numstat`).
- Recovery — Opus recovery assessment, incremental vs full re-decomposition, recovery round budgets inherited to prevent exponential growth.
- **Usage tracking** — `TaskUsage` on each task accumulates tokens, cost, tool calls, and API latency across all agent phases. `SessionMeta` + `AgentResult<T>` wrapper propagates per-call metadata from reel through `AgentService` to the orchestrator. `UsageUpdated` event drives real-time TUI updates. `EpicState::total_usage()` aggregates across all tasks. Usage persisted in `state.json` via `#[serde(default)]` (backward-compatible). TUI metrics panel shows cost. Header shows running cost. Headless and `epic status` print usage summary with cache hit ratio.
- **File-level review** — Leaf tasks undergo a separate semantic review after verification gates pass. Catches requirement/intent mismatches that build/lint/test cannot detect. Runs between verification pass and task completion. Model: `max(Haiku, implementing_model)` capped at Sonnet (reuses `verification_model()`). On failure, feeds into the existing leaf fix loop. Skipped for branch tasks. Fix tasks that fail review are failed immediately (no recursive fix loop).
- Event system — 24 event variants driving TUI.
- CI pipeline — GitHub Actions (fmt, clippy, test, build) on ubuntu, macOS, Windows. Rust 1.93.1 toolchain. All epic jobs green on all platforms. Dependencies use pinned git revs (lot, reel, vault, flick).
- Testability infrastructure — `ClientFactory`/`ToolExecutor` traits (reel, internal), `git_diff_numstat` extraction (orchestrator), shared `MockAgentService` (`test_support`), `TaskPhase::try_transition`, `PartialEq` on `LeafResult`/`RecoveryPlan`, stdin injection in init
- **Vault integration** — Document store via `vault` crate (git rev `f7ecea1`). `VaultConfig` in `epic.toml` (`[vault]` section, `enabled = false` by default). Vault constructed at startup, bootstrapped on new runs. `ResearchQuery` custom tool (reel `ToolHandler`) injected into execute, decompose, fix, and recovery design phases — agents query accumulated project knowledge on demand. Discovery recording at 4 orchestrator integration points (leaf discoveries, verification failures, checkpoint adjust, recovery). Vault reorganize runs after root branch children complete. Usage tracking folds vault costs into per-task `TaskUsage`. Vault events drive TUI worklog. All vault operations are best-effort (failures logged, not propagated).
- **Research Service gap-filling** — `ResearchQuery` tool implements a multi-step pipeline: (1) query vault for existing knowledge, (2) identify information gaps via Haiku structured-output call, (3) fill gaps by spawning Haiku agents with read-only tools to explore the project codebase, (4) synthesize final answer combining vault knowledge and exploration findings. Optional `scope` parameter: `vault` (stored knowledge only) or `project` (default, vault + codebase exploration). Exploration findings are recorded back into vault. All internal agent calls use Haiku ("fast" model key). Returns structured `ResearchResult { answer, document_refs, gaps_filled }`. Web search scope deferred.
- **Test counts** — 258 tests (all pass).

## What Is NOT Implemented

These features are described in DESIGN.md but have no corresponding code:

- **Simplification review** — No local simplification review on leaf output, no aggregate simplification review on branch output. Both deferred.
- **Branch verification separation** — Branch verification is a single agent call, not separated into correctness + completeness + aggregate simplification reviews as described in DESIGN.md.
- **User-level config fallback** — Only project-level config (`epic.toml`, `.epic/config.toml`) is loaded. No `~/.config/epic/config.toml` resolution.

## Design Choices (intentional constraints)

### Sequential execution only

Epic executes subtasks sequentially by design. Simplifies implementation, keeps TUI output and logging coherent, and prioritizes cost control and correctness over throughput while the design matures.

### No multi-language special handling

Epic uses generalized prompts that work across languages. No language-specific logic.

### No git hosting integration

No GitHub/GitLab PR creation, issue tracking, or similar integrations.

## Completed Work

### Reel Extraction

Agent session layer extracted into standalone `reel` crate. Epic is now a thin consumer. See [REEL_EXTRACTION.md](REEL_EXTRACTION.md) for the original spec.

### CI Pipeline Fix

Replaced local path dependencies with pinned git rev dependencies so CI builds work in isolation on all platforms. Added `.gitattributes` with `eol=lf` to eliminate cross-platform `rustfmt` divergence. Fixed clippy lints for newer toolchain. Fixed `reel_config.nu` compatibility with nu 0.111.0 (`str replace --string` flag removed). Fixed lot sandbox policy to allow write-path children under read-path parents (needed for session temp dirs inside read-only project roots).

### Reel Upgrade and Usage Tracking

Bumped reel from rev `51eb559` to `93f35ef`, picking up session transcripts, cache token fields, and per-call API latency. Added `TaskUsage` type, `SessionMeta`/`AgentResult<T>` wrapper, changed all 9 `AgentService` methods to return metadata alongside domain results. Orchestrator accumulates usage at all 10 agent call sites. `UsageUpdated` event feeds TUI metrics panel. Headless and `epic status` output usage summary. Transcript persistence deferred (reel's `TurnRecord` does not derive `Serialize`).

### Vault Integration

Integrated the `vault` crate (git rev `f7ecea1`) as epic's document store and research service. Vault is a file-based knowledge store at `.epic/docs/` with four operations (bootstrap, record, query, reorganize) backed by a reel agent (librarian). Integration points: `VaultConfig` in `epic.toml`, vault construction and bootstrap in `main.rs`, `ResearchQuery` custom tool via reel `ToolHandler` injected into 5 agent phases, discovery recording at 4 orchestrator sites, vault reorganize before root verification, usage tracking via `SessionMeta::from_vault`, 3 vault event variants for TUI. All vault operations are best-effort.

### Research Service Gap-Filling

Extended `ResearchQuery` from vault-query-only to a multi-step gap-filling pipeline: vault query → gap identification (Haiku structured output) → codebase exploration (Haiku with read-only tools, capped at 5 gaps) → synthesis. Added `ResearchScope` enum with optional `scope` tool parameter (`vault`/`project`). Graceful degradation at each step. Exploration findings recorded back to vault. `run_haiku<T>()` generic helper and `vault_only_result()` helper keep the implementation DRY. `Arc<reel::Agent>` shared between `ReelAgent` and `ResearchTool`. 24 tests in knowledge module.

### File-Level Review

Added file-level review as a leaf verification sub-phase. After verification gates pass for a leaf task, a separate agent call reviews the actual source file changes for intent/requirement alignment. Reuses `VerificationResult`/`VerificationWire`/`verification_schema()` types and `verification_model()` model selection. On failure, feeds into the existing leaf fix loop (or fails immediately for fix tasks). Branch tasks skip file-level review. `try_file_level_review()` helper called from both `finalize_task` (initial execution) and `try_verify` (fix loop re-verification). New `FileLevelReviewCompleted` event variant. `build_file_level_review` prompt builder. 4 new orchestrator tests.

## Source Summary

24 files, 14,657 lines. Test code (58%) outweighs core (42%).

```
src/                              Total   Core   Test
├── main.rs                         359    357      2
├── orchestrator.rs               7,345  1,494  5,851
├── knowledge.rs                    969    612    357
├── state.rs                        428    115    313
├── events.rs                       118    118      0
├── cli.rs                           69     46     23
├── init.rs                         582    347    235
├── sandbox.rs                      262    132    130
├── test_support.rs                 232      0    232
├── agent/
│   ├── mod.rs                      184    184      0
│   ├── prompts.rs                  879    510    369
│   ├── reel_adapter.rs             497    432     65
│   └── wire.rs                     731    414    317
├── config/
│   ├── mod.rs                        3      3      0
│   └── project.rs                  637    294    343
├── task/
│   ├── mod.rs                      373    226    147
│   ├── assess.rs                    12     12      0
│   ├── branch.rs                    24     24      0
│   ├── leaf.rs                       2      2      0
│   └── verify.rs                    19     19      0
└── tui/
    ├── mod.rs                      620    503    117
    ├── task_tree.rs                134    134      0
    ├── metrics.rs                   96     96      0
    └── worklog.rs                   82     82      0
                                 ──────  ─────  ─────
                                 14,657  6,156  8,501
```

**Classification**: "Test" = lines inside `#[cfg(test)]` modules + `test_support.rs` (entirely test infrastructure). "Core" = everything else. All source is in `src/` — no `build.rs`, `tests/`, `benches/`, or `examples/` directories.

**Notable**: `orchestrator.rs` is 50% of all source and 69% of all test code. `test_support.rs` is a shared mock `AgentService` gated behind `#[cfg(test)]`.

## Work Candidates

### 1. Branch Verification Separation

Branch verification is currently a single agent call. Splitting into correctness + completeness + aggregate simplification reviews (as designed in DESIGN.md) would improve branch-level quality and make fix-loop targeting more precise.

### 2. Web Search Scope for Research Service

The Research Service gap-filling pipeline is implemented for PROJECT scope (vault + codebase exploration). WEB scope (web search to fill gaps that codebase exploration cannot) is deferred. Adding it requires a web search tool grant and integration with a search provider.

### 3. User-Level Config Fallback

Only project-level config (`epic.toml`, `.epic/config.toml`) is loaded. No `~/.config/epic/config.toml` resolution for user defaults.
