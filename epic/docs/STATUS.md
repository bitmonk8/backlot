# Project Status

## Current Phase

**Core orchestration implemented. Knowledge layer not started.**

## What Is Implemented

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `ReelAgent` implementing `AgentService` (9 methods) — thin adapter building `reel::AgentRequestConfig` per phase, delegates tool loop and tool execution to reel crate
- **Reel crate** (`../reel/reel`, rev `93f35ef`) — standalone agent session layer extracted from epic. Contains: `Agent` runtime (tool loop with resume), 6 built-in tools (`Read`, `Write`, `Edit`, `Glob`, `Grep`, `NuShell`), `NuSession` (persistent `nu --mcp` process inside lot sandbox), `ToolHandler` trait for custom tool dispatch, `ToolGrant` bitflags (WRITE/TOOLS/NETWORK), `ModelRegistry`/`ProviderRegistry` re-exports from flick. Nu config — `reel_config.nu` and `reel_env.nu` written to `target/nu-cache/` by `build.rs`, custom commands (`reel read`, `reel write`, `reel edit`, `reel glob`, `reel grep`). `REEL_RG_PATH` env var for rg binary injection. `RunResult` exposes `Usage` (tokens + cost), `TurnRecord` transcript, and per-call API latency.
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
- Event system — 20 event variants driving TUI and JSONL logging.
- CI pipeline — GitHub Actions (fmt, clippy, test, build) on ubuntu, macOS, Windows. Rust 1.93.1 toolchain. All epic jobs green on all platforms. Dependencies use pinned git revs (lot, reel, flick).
- Testability infrastructure — `ClientFactory`/`ToolExecutor` traits (reel, internal), `git_diff_numstat` extraction (orchestrator), shared `MockAgentService` (`test_support`), `TaskPhase::try_transition`, `PartialEq` on `LeafResult`/`RecoveryPlan`, stdin injection in init
- **Test counts** — Epic: 223 tests (all pass). Reel: 142 pass, 3 fail (AppContainer sandbox access issues in `reel read`/`write`/`edit` custom command tests — see `reel/docs/ISSUES.md` #9c).

## What Is NOT Implemented

These features are described in DESIGN.md but have no corresponding code:

- **Document Store** — No `.epic/docs/` persistence, no librarian agent, no bootstrap/query/record operations. Discoveries exist only in-memory on `task.discoveries`.
- **Research Service** — No `research_query` tool, no gap-filling via web search or codebase exploration, no integration with document store. Not exposed as a tool during any agent phase.
- **File-level review** — Leaf verification does not include a separate file-level review step. Deferred per code comment in `verify.rs`.
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

Agent session layer extracted into standalone `reel` workspace at `../reel/` (library crate at `../reel/reel`). Epic is now a thin consumer. See [REEL_EXTRACTION.md](REEL_EXTRACTION.md) for the original spec.

### CI Pipeline Fix

Replaced local path dependencies (`../lot`, `../reel/reel`) with pinned git rev dependencies so CI builds work in isolation on all platforms. Added `.gitattributes` with `eol=lf` to eliminate cross-platform `rustfmt` divergence. Fixed clippy lints for newer toolchain. Fixed `reel_config.nu` compatibility with nu 0.111.0 (`str replace --string` flag removed). Fixed lot sandbox policy to allow write-path children under read-path parents (needed for session temp dirs inside read-only project roots).

### Reel Upgrade and Usage Tracking

Bumped reel from rev `51eb559` to `93f35ef`, picking up session transcripts, cache token fields, and per-call API latency. Added `TaskUsage` type, `SessionMeta`/`AgentResult<T>` wrapper, changed all 9 `AgentService` methods to return metadata alongside domain results. Orchestrator accumulates usage at all 10 agent call sites. `UsageUpdated` event feeds TUI metrics panel. Headless and `epic status` output usage summary. Transcript persistence deferred (reel's `TurnRecord` does not derive `Serialize`).

## Work Candidates

(none)
