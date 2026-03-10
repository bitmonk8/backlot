# Project Status

## Current Phase

**v1 complete** — All features implemented.

## What Exists

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `FlickAgent` implementing `AgentService` via Flick library crate — config generation, structured output schemas, prompt assembly, tool loop with resume
- 6 tools: `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `nu` — path sandboxing, size limits, timeout handling
- State persistence via `.epic/state.json` — atomic writes, resume, goal mismatch detection, corrupt state handling, cycle-safe DFS
- TUI via ratatui + crossterm — task tree, worklog, metrics panels, keyboard controls
- CLI via clap — `init`, `run <goal>`, `resume`, `status` subcommands
- `epic init` — agent-driven interactive configuration scaffolding
- Container/VM startup detection with suppressible warning
- Process sandboxing via lot — nu tool runs inside a persistent `nu --mcp` process spawned inside an OS-native sandbox (AppContainer on Windows, namespaces+seccomp on Linux, Seatbelt on macOS); one nu MCP session per agent call, sandbox is mandatory (no unsandboxed fallback)
- CI pipeline — GitHub Actions (fmt, clippy, test, build), Rust 1.93.1 toolchain, Flick pinned to rev `f83c56e`
- Testability infrastructure — `ProviderResolver`/`ToolExecutor` traits (flick), `git_diff_numstat` extraction (orchestrator), shared `MockAgentService` (`test_support`), `TaskPhase::try_transition`, `PartialEq` on `LeafResult`/`RecoveryPlan`, stdin injection in init

## Design Choices (intentional constraints)

### Sequential execution only

Epic executes subtasks sequentially by design. Simplifies implementation, keeps TUI output and logging coherent, and prioritizes cost control and correctness over throughput while the design matures.

### No multi-language special handling

Epic uses generalized prompts that work across languages. No language-specific logic.

### No git hosting integration

No GitHub/GitLab PR creation, issue tracking, or similar integrations in v1.

## Next Work Candidates

1. **Unified nu tool layer** — Move file tools into the sandboxed nu MCP session as nu custom commands. Agent-facing tool schemas aligned with Claude Code's tool interface (Read, Write, Edit, Glob, Grep, Bash) — Claude models are trained on these schemas. Epic translates JSON tool calls to nu commands internally; agents never write nu syntax for file ops. Eliminates TOCTOU races, removes `safe_path()` and `ToolGrant::READ/WRITE`, makes lot the sole access control mechanism. Spec: `docs/SPEC_NU_UNIFIED_TOOLS.md` (decisions D1-D6 recorded, 5-phase implementation plan). Next step: Phase 1 prototyping (validate nu custom commands and rg integration against nu 0.111.0).
2. **Nu integration tests** — No integration tests for the nu MCP session (spawn, timeout, kill, env filtering, exit codes). Protocol parsing functions (`try_parse_response`, `read_response`) and generation-based session invalidation also lack unit tests. Could be combined with unified tool layer work.
