# Project Status

## Current Phase

**Core orchestration implemented. Knowledge layer not started.**

## What Is Implemented

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `FlickAgent` implementing `AgentService` (9 methods) via Flick library crate — config generation, structured output schemas, prompt assembly, tool loop with resume
- 6 tools: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `NuShell` — Claude Code-aligned schemas executed as nu custom commands via `translate_tool_call()` / `format_tool_result()`. All tool execution routes through `execute_tool()` → nu MCP session.
- Nu config integration — `epic_config.nu` and `epic_env.nu` written to `target/nu-cache/` by `build.rs`, loaded via `nu --mcp --config <path> --env-config <path>`. Custom commands (`epic read`, `epic write`, `epic edit`, `epic glob`, `epic grep`) available immediately in MCP sessions without evaluate preamble. `EPIC_RG_PATH` env var injects rg binary absolute path into nu session; `epic grep` uses `^$env.EPIC_RG_PATH` for direct invocation (bypasses nu PATH splitting issues under AppContainer). Sandbox policy grants exec access to cache dir for config files and rg binary.
- State persistence via `.epic/state.json` — atomic writes, resume, goal mismatch detection, corrupt state handling, cycle-safe DFS
- TUI via ratatui + crossterm — task tree, worklog, metrics panels, keyboard controls
- CLI via clap — `init`, `run <goal>`, `resume`, `status`, `setup` subcommands
- `epic init` — agent-driven interactive configuration scaffolding
- Container/VM startup detection with suppressible warning
- Process sandboxing via lot — nu tool runs inside a persistent `nu --mcp` process spawned inside an OS-native sandbox (AppContainer on Windows, namespaces+seccomp on Linux, Seatbelt on macOS); one nu MCP session per agent call, sandbox is mandatory (no unsandboxed fallback). `epic setup` grants AppContainer prerequisites (NUL device ACL + ancestor traverse ACEs) via one-time elevated operation; `run`/`resume` check `appcontainer_prerequisites_met(&[project_root])` and fail early if not configured.
- Context propagation — `TaskContext` carries discoveries, parent goals, sibling summaries, checkpoint guidance. Structural map injection in prompts (ancestor chain, completed/pending siblings).
- Discovery flow — in-memory tracking via `task.discoveries`. Inter-subtask checkpoint with Haiku classification (proceed/adjust/escalate). Discovery bubbling to parent.
- Assessment — Haiku call returns path (leaf/branch) + model selection. Root forced to branch, max-depth forced to leaf.
- Verification & fix loops — leaf fix loop with model escalation (Haiku→Sonnet→Opus, 3 retries per tier), branch fix loop (3 Sonnet rounds + 1 Opus round for root), scope circuit breaker (3x magnitude estimate via `git diff --numstat`).
- Recovery — Opus recovery assessment, incremental vs full re-decomposition, recovery round budgets inherited to prevent exponential growth.
- Event system — 19 event variants driving TUI and JSONL logging.
- CI pipeline — GitHub Actions (fmt, clippy, test, build), Rust 1.93.1 toolchain, Flick pinned to rev `b36e0f3`
- Testability infrastructure — `ClientFactory`/`ToolExecutor` traits (flick), `git_diff_numstat` extraction (orchestrator), shared `MockAgentService` (`test_support`), `TaskPhase::try_transition`, `PartialEq` on `LeafResult`/`RecoveryPlan`, stdin injection in init
- Nu session tests — 19 unit tests (protocol parsing, session state, generation invalidation, config resolution) and 23 integration tests (spawn lifecycle, custom command availability, timeout handling, grant change respawn, env filtering, error handling, sandbox policy verification: read-only write prevention, rg child process execution, temp dir pivot prevention, write grant verification). Sandbox tests use per-test isolated cache dirs to avoid concurrent ACL conflicts.

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

## Priority 1: Reel Extraction

Extracting the agent session layer (tool loop, tool definitions, NuSession, sandboxing) into a separate `reel` crate. See [REEL_EXTRACTION.md](REEL_EXTRACTION.md) for the spec.

**Design status**: Complete — `Agent`, `AgentEnvironment`, `AgentRequest`, `RunResult`, `ToolHandler` trait for custom tool dispatch.

**Flick named models**: Done. Epic migrated to `ModelRegistry`/`RequestConfig` API (commit `614f6b6`).

**Next step**: Create reel crate, move code, wire epic as consumer. All nu session test categories resolved — reel extraction is unblocked.

## Other Work Candidates

1. **`quote_nu()` adversarial input tests** — Missing adversarial cases: subshell expressions `$(...)`, null bytes, and multi-line strings containing closing delimiters. Sandbox limits blast radius, but injection causes confusing errors.
2. **Remove unused crate dependencies** — `globset`, `walkdir`, `regex` are unused. Blocked by Rust 1.93.1 compiler ICE triggered by `windows-sys 0.61.2` when these are removed. Revisit when toolchain updates.
