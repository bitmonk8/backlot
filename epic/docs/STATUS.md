# Project Status

## Current Phase

**v1 complete** — All features implemented. Unified tool layer Phases 1-3 complete; Phase 4 (legacy removal) pending.

## What Exists

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `FlickAgent` implementing `AgentService` via Flick library crate — config generation, structured output schemas, prompt assembly, tool loop with resume
- 6 tools: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `NuShell` — Claude Code-aligned schemas forwarding to nu custom commands via `translate_tool_call()` / `format_tool_result()`. Configurable via `[agent] file_tool_forwarders` (default: true). Legacy Rust-native implementations (`read_file`, `glob`, `grep`, `write_file`, `edit_file`, `nu`) retained temporarily; slated for removal in Phase 4.
- Nu config integration — `epic_config.nu` and `epic_env.nu` written to `target/nu-cache/` by `build.rs`, loaded via `nu --mcp --config <path> --env-config <path>`. Custom commands (`epic read`, `epic write`, `epic edit`, `epic glob`, `epic grep`) available immediately in MCP sessions without evaluate preamble. `EPIC_RG_DIR` env var injects rg binary path into nu session; `epic_env.nu` prepends it to PATH. Sandbox policy grants exec access to cache dir for config files and rg binary.
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

1. **Unified nu tool layer — Phase 4: Remove old tool layer** — Phases 1-3 complete. Remove `tool_read_file`, `tool_write_file`, `tool_edit_file`, `tool_glob`, `tool_grep` from `tools.rs`. Remove `safe_path()`, `verify_ancestors_within_root()`. Simplify `ToolGrant` to phase marker. Remove legacy `nu` tool name. Update prompt assembly (`prompts.rs`) with new tool names and descriptions.
2. **Nu integration tests** — No integration tests for the nu MCP session (spawn, timeout, kill, env filtering, exit codes). Protocol parsing functions (`try_parse_response`, `read_response`) and generation-based session invalidation also lack unit tests. Integration tests should verify custom commands are available immediately after spawn (Phase 3 validation).
