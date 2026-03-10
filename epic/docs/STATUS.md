# Project Status

## Current Phase

**v1 complete** — All features implemented. Unified tool layer complete (Phases 1-4).

## What Exists

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `FlickAgent` implementing `AgentService` via Flick library crate — config generation, structured output schemas, prompt assembly, tool loop with resume
- 6 tools: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `NuShell` — Claude Code-aligned schemas executed as nu custom commands via `translate_tool_call()` / `format_tool_result()`. All tool execution routes through `execute_tool()` → nu MCP session.
- Nu config integration — `epic_config.nu` and `epic_env.nu` written to `target/nu-cache/` by `build.rs`, loaded via `nu --mcp --config <path> --env-config <path>`. Custom commands (`epic read`, `epic write`, `epic edit`, `epic glob`, `epic grep`) available immediately in MCP sessions without evaluate preamble. `EPIC_RG_DIR` env var injects rg binary path into nu session; `epic_env.nu` prepends it to PATH. Sandbox policy grants exec access to cache dir for config files and rg binary.
- State persistence via `.epic/state.json` — atomic writes, resume, goal mismatch detection, corrupt state handling, cycle-safe DFS
- TUI via ratatui + crossterm — task tree, worklog, metrics panels, keyboard controls
- CLI via clap — `init`, `run <goal>`, `resume`, `status` subcommands
- `epic init` — agent-driven interactive configuration scaffolding
- Container/VM startup detection with suppressible warning
- Process sandboxing via lot — nu tool runs inside a persistent `nu --mcp` process spawned inside an OS-native sandbox (AppContainer on Windows, namespaces+seccomp on Linux, Seatbelt on macOS); one nu MCP session per agent call, sandbox is mandatory (no unsandboxed fallback)
- CI pipeline — GitHub Actions (fmt, clippy, test, build), Rust 1.93.1 toolchain, Flick pinned to rev `f83c56e`
- Testability infrastructure — `ProviderResolver`/`ToolExecutor` traits (flick), `git_diff_numstat` extraction (orchestrator), shared `MockAgentService` (`test_support`), `TaskPhase::try_transition`, `PartialEq` on `LeafResult`/`RecoveryPlan`, stdin injection in init
- Nu session tests — 21 unit tests (protocol parsing, session state, generation invalidation) and 16 integration tests (spawn lifecycle, custom command availability, timeout handling, grant change respawn, env filtering, error handling)

## Design Choices (intentional constraints)

### Sequential execution only

Epic executes subtasks sequentially by design. Simplifies implementation, keeps TUI output and logging coherent, and prioritizes cost control and correctness over throughput while the design matures.

### No multi-language special handling

Epic uses generalized prompts that work across languages. No language-specific logic.

### No git hosting integration

No GitHub/GitLab PR creation, issue tracking, or similar integrations in v1.

## Next Work Candidates

1. **Sandbox policy verification (Phase 5)** — Three verification items from the unified tool layer spec remain untested:
   - Verify lot `read_path` prevents writes on all platforms (Linux, macOS, Windows)
   - Verify rg binary is accessible within lot sandbox on all platforms
   - Verify temp dir access cannot pivot to project root (agent copies file to temp, attempts write-back under read-only policy)
2. **`quote_nu()` adversarial input tests** — The translation layer's `quote_nu()` has unit tests for common special characters (single/double quotes, backticks, newlines, backslashes, dollar signs, raw string delimiters). Missing adversarial cases: subshell expressions `$(...)`, null bytes, and multi-line strings containing closing delimiters. Sandbox limits blast radius, but injection causes confusing errors.
3. **Remove unused crate dependencies** — `globset`, `walkdir`, `regex` are now unused after legacy tool removal. Blocked by Rust 1.93.1 compiler ICE triggered by `windows-sys 0.61.2` when these are removed. Revisit when toolchain updates.
