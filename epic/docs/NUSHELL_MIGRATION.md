# NuShell Migration Spec

Replace POSIX `sh` with [NuShell](https://www.nushell.sh/) as epic's sole shell runtime, using a persistent MCP session.

## Motivation

POSIX `sh` is unix-centric. On Windows it requires Git Bash or similar, and behavior diverges across platforms. NuShell is cross-platform with consistent behavior on Windows, Linux, and macOS.

Beyond cross-platform consistency, a persistent MCP session gives epic stateful shell access — environment variables, working directory, and shell state carry across tool calls. The current approach spawns a fresh `sh` per tool call, losing all state each time.

## Approach

Epic builds a custom `nu` binary from NuShell's Rust crates with MCP support enabled. At runtime, epic spawns `nu --mcp` once, keeps it running, and communicates via MCP protocol over stdio. Lot sandboxes the spawned `nu` process — same OS-level isolation as today.

## NuShell MCP Server

NuShell has built-in MCP server support since v0.108.0 (October 2025). Default feature since v0.110.0.

### CLI Arguments

| Flag | Since | Purpose |
|------|-------|---------|
| `nu --mcp` | v0.108.0 | Start MCP server (stdio transport) |
| `--mcp-transport http` | v0.111.0 | Use HTTP instead of stdio |
| `--mcp-port <PORT>` | v0.111.0 | HTTP port (default 8080) |

Epic uses stdio transport (`nu --mcp`).

### MCP Tools Exposed

- **evaluate** — Execute arbitrary NuShell commands/pipelines. Primary tool.
- **find_command / list_command** — Discover available NuShell commands.

### Structured Responses (v0.110.0+)

```json
{ "history_index": 5, "cwd": "/home/user/project", "output": "..." }
```

Error formatting is LLM-friendly (ANSI coloring disabled, rich error diagnostics with line/column details).

### Session Persistence (v0.110.0+)

Variables, environment variables, and working directory persist across MCP tool calls. A `$history` variable provides access to command history.

## Custom NuShell Build

Epic ships its own `nu` binary — no separate install required.

### Required Crates

| Crate | Purpose |
|---|---|
| `nu-cli` | Entry points including MCP server startup |
| `nu-cmd-lang` | `create_default_context()`, language builtins (`def`, `let`, `if`, `for`) |
| `nu-command` | ~350 built-in commands (filesystem, string, math, networking) |
| `nu-engine` | AST evaluator, command dispatch |
| `nu-parser` | Source → AST |
| `nu-protocol` | Core types (`EngineState`, `Stack`, `Value`, `PipelineData`) |
| `nu-std` | Standard library scripts |
| `nu-mcp` | MCP server implementation |

All crates are pinned to an exact NuShell release (currently 0.111.1, MSRV 1.92.0) via `=` version constraints in `epic-nu/Cargo.toml`. Epic ships its own `epic-nu` binary — no separate NuShell install required. NuShell version updates are deliberate: bump the pinned version, run CI, release.

### Feature Flags

Disable `plugin`, `dataframe`, `sqlite`, `trash-support` to minimize binary size. Keep `mcp` and `network` enabled.

### Binary Size

| Configuration | Size |
|---|---|
| Full (stripped) | ~37-38 MB |
| Minimal features (stripped) | ~14-18 MB (estimated) |

### Build Approach

Add a workspace member binary crate (`epic-nu/`) that depends on the NuShell crates and produces an `epic-nu` binary. Epic's build produces two binaries: `epic` and `epic-nu`. At runtime, epic spawns `epic-nu --mcp`.

Clean separation, standard Cargo workflow, easy to version-lock nushell crates.

## MCP Client in Epic

Epic needs an MCP client to communicate with the `nu --mcp` process. This replaces the current spawn-per-call pattern.

**Protocol**: JSON-RPC 2.0 over stdio. Epic writes JSON-RPC requests to nu's stdin, reads JSON-RPC responses from nu's stdout.

**Tool dispatch**: Instead of spawning a process per call, `tool_nu` sends an MCP `tools/call` request to the persistent nu process with tool name `"evaluate"` and the command string as the argument.

**Response parsing**: MCP responses are structured JSON (`{ history_index, cwd, output }`), replacing raw stdout/stderr capture.

### Flick Integration

Epic's `FlickAgent` dispatches tool calls via `execute_tool()`. The MCP-based nu tool changes the internal implementation of `tool_nu` but not the `execute_tool` interface. Flick sees the same tool definition (`"nu"` with `command` parameter) — only the backend changes.

No changes to Flick itself. The MCP client lives in epic's tool implementation layer.

## Session Lifecycle

One nu MCP process per agent session. Each agent session has a fixed phase (Decompose, Execute, or Verify) and therefore a fixed sandbox policy. The nu process is spawned lazily at the session's first tool call and killed when the session returns structured output.

Agent sessions are oneshot — once a session returns, it is never reused. A new session (possibly with a different phase/policy) gets a fresh nu process. This means:

- No phase-change restart logic needed. Session = phase = sandbox policy = nu process lifetime.
- Multiple tool calls within a session share the nu process and its state (env vars, cwd, variables).
- Sandbox correctness is guaranteed by construction — each nu process runs under the exact policy its phase requires.

## Current State

The shell tool is implemented in `src/agent/tools.rs`:

- Tool name: `"bash"`
- Shell binary: `"sh"` invoked with `-c <command>` (lines 649, 714)
- Two execution paths: sandboxed (lot `SandboxCommand`) and unsandboxed fallback (`tokio::process::Command`)
- Grant flag: `ToolGrant::BASH` (bitflag `0b0000_0100`)
- Available in Execute and Decompose phases; not in Analyze
- Constants: `MAX_BASH_OUTPUT` (64 KiB), `DEFAULT_BASH_TIMEOUT_SECS` (120), `MAX_BASH_TIMEOUT_SECS` (600)
- Internal types: `BashOutput` struct, functions `tool_bash`, `tool_bash_sandboxed`, `tool_bash_unsandboxed`, `format_bash_output`, `bash_output_from`
- Process group management: `setsid()` on Unix, `CREATE_NEW_PROCESS_GROUP` on Windows — shell-agnostic
- Environment: `env_clear()` + whitelist (`UNSANDBOXED_ENV_KEYS`)
- Tests: ~15 bash-specific tests (lines 1192–1419)

## Required Changes

### New: `epic-nu/` workspace member

Binary crate producing `epic-nu`. Depends on NuShell crates listed above. Single `main.rs` that boots the NuShell engine and starts the MCP server.

### New: MCP client module

Module in epic (e.g. `src/agent/nu_session.rs`) that manages the `epic-nu --mcp` process for an agent session:

- Spawn (lazily on first tool call) and kill (when session ends) lifecycle
- JSON-RPC request/response serialization
- Timeout handling per MCP call

### `src/agent/tools.rs`

| Item | Current | New |
|---|---|---|
| Tool name | `"bash"` | `"nu"` |
| Tool description | `"Execute a bash command..."` | `"Execute a NuShell command..."` |
| Implementation | Spawn `sh -c` per call | Send MCP `evaluate` to persistent nu session |
| Constants | `MAX_BASH_OUTPUT`, `DEFAULT_BASH_TIMEOUT_SECS`, `MAX_BASH_TIMEOUT_SECS` | `MAX_NU_OUTPUT`, `DEFAULT_NU_TIMEOUT_SECS`, `MAX_NU_TIMEOUT_SECS` |
| Struct | `BashOutput` | `NuOutput` |
| Functions | `tool_bash`, `tool_bash_sandboxed`, `tool_bash_unsandboxed`, `format_bash_output`, `bash_output_from` | `tool_nu`, `format_nu_output` (sandboxed/unsandboxed distinction moves to session layer) |
| Grant flag | `ToolGrant::BASH` | `ToolGrant::NU` |

Tests: rename, update command syntax (e.g. `sleep 10` → `sleep 10sec`), add MCP session lifecycle tests.

### Unchanged subsystems

- **Sandbox policy**: lot `SandboxPolicy` is shell-agnostic. `build_sandbox_policy()` unchanged.
- **Output handling**: exit code convention (0 = success) preserved. MCP structured responses provide richer data but `NuOutput` maps to same interface.
- **Process group management**: Applied to the persistent nu process instead of per-call processes.

### Other source files

- `src/agent/config_gen.rs` — No changes. Tool list generated dynamically.
- `src/agent/prompts.rs` — No changes. Tool name and description tell the model which shell to use.
- `src/config/project.rs`, `src/init.rs` — No changes. `VerificationStep.command` is `Vec<String>`.

### Documentation

| File | Change |
|---|---|
| `docs/STATUS.md` | `bash` → `nu`, add MCP session description |
| `docs/SANDBOXING.md` | "bash tool" → "nu tool", note persistent process model |
| `docs/CONFIGURATION.md` | Update any bash references |
| `docs/LOT_SPEC.md` | Update if `sh` appears as example program |
| `docs/audit/X6.md` | `DEFAULT_BASH_TIMEOUT_SECS` → `DEFAULT_NU_TIMEOUT_SECS` |
| `docs/audit/U5-R2.md` | "bash tool" → "nu tool" |
| `docs/audit/U1-R2.md` | "bash tool" → "nu tool" |
| `README.md` | Tool table: `bash` → `nu` |
| `Cargo.toml` | Add `epic-nu` to workspace members |

## NuShell Compatibility Notes

1. **Exit codes**: Standard (0 = success). MCP responses include exit status.
2. **Environment variables**: NuShell reads them. The `env_clear()` + whitelist approach works at process spawn.
3. **Process signals**: NuShell responds to SIGKILL / TerminateProcess normally.
4. **Syntax**: LLM agents generate NuShell syntax instead of POSIX sh. The tool name and description handle this implicitly.

## Timeout Handling

On timeout, epic kills the nu MCP process entirely and returns an error to the agent indicating the session was terminated. The next tool call spawns a fresh nu session (session state is lost). The agent recovers naturally — the error message tells it what happened, and it proceeds with a clean session. No MCP-level request cancellation needed.

## References

- [NuShell GitHub](https://github.com/nushell/nushell) — Source, 37-crate workspace, version 0.111.1
- [NuShell 0.108.0 release](https://www.nushell.sh/blog/2025-10-15-nushell_v0_108_0.html) — Initial MCP
- [NuShell 0.110.0 release](https://www.nushell.sh/blog/2026-01-17-nushell_v0_110_0.html) — MCP default, state persistence
- [NuShell 0.111.0 release](https://www.nushell.sh/blog/2026-02-28-nushell_v0_111_0.html) — HTTP transport, request cancellation
- [NuShell MCP issue #15435](https://github.com/nushell/nushell/issues/15435) — Original MCP proposal
