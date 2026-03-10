# Project Status

## Current Phase

**v1 complete** — All features implemented. Unified tool layer in progress.

## What Exists

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `FlickAgent` implementing `AgentService` via Flick library crate — config generation, structured output schemas, prompt assembly, tool loop with resume
- 6 tools: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `NuShell` — Claude Code-aligned schemas forwarding to nu custom commands via `translate_tool_call()` / `format_tool_result()`. Configurable via `[agent] file_tool_forwarders` (default: true). Legacy Rust-native implementations (`read_file`, `glob`, `grep`, `write_file`, `edit_file`, `nu`) retained temporarily; slated for removal in Phase 4.
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

1. **Unified nu tool layer — Phase 3: Config file integration** — Phases 1-2 complete. Nu custom commands validated, ripgrep added to build, translation layer implemented with 27 unit tests. **Next step**: Embed `epic_config.nu` and `epic_env.nu` as Rust const strings, modify `NuSession` to spawn `nu --mcp --config <path> --env-config <path>`, test that forwarded tool calls work end-to-end through nu.
2. **Nu integration tests** — No integration tests for the nu MCP session (spawn, timeout, kill, env filtering, exit codes). Protocol parsing functions (`try_parse_response`, `read_response`) and generation-based session invalidation also lack unit tests. Could be combined with unified tool layer work.
