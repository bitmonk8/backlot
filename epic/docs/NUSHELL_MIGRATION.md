# NuShell Migration Spec — DONE

Replace POSIX `sh` with [NuShell](https://www.nushell.sh/) as epic's sole shell runtime, using a persistent MCP session. Implementation complete.

## Motivation

POSIX `sh` is unix-centric. On Windows it requires Git Bash or similar, and behavior diverges across platforms. NuShell is cross-platform with consistent behavior on Windows, Linux, and macOS.

Beyond cross-platform consistency, a persistent MCP session gives epic stateful shell access — environment variables, working directory, and shell state carry across tool calls. The previous approach spawned a fresh `sh` per tool call, losing all state each time.

## Approach

Epic's `build.rs` downloads a prebuilt NuShell 0.111.0 binary from GitHub releases, verifies its SHA-256 checksum, and caches it in `target/nu-cache/`. At runtime, epic resolves the `nu` binary by checking: (1) same directory as the epic executable, (2) build-time cache, (3) `PATH`. Epic then spawns `nu --mcp` once, keeps it running, and communicates via MCP protocol over stdio. Lot sandboxes the spawned `nu` process — same OS-level isolation as today.

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

## NuShell Binary

Epic ships a prebuilt `nu` binary — no separate install required. No NuShell crates are compiled from source.

Epic downloads a prebuilt NuShell 0.111.0 binary at build time (via `build.rs`). The binary is verified against a hardcoded SHA-256 checksum and cached in `target/nu-cache/`. Version updates are deliberate: update the version and checksums in `build.rs`, run CI, release.

At runtime, epic resolves the `nu` binary by checking: (1) same directory as the epic executable, (2) build-time cache, (3) `PATH`. Epic then spawns `nu --mcp`.

## MCP Client in Epic

Epic uses an MCP client to communicate with the `nu --mcp` process, replacing the former spawn-per-call bash pattern.

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

## Implementation Summary

| Component | Location | What Changed |
|---|---|---|
| NuShell binary | `build.rs` + `target/nu-cache/` | Prebuilt NuShell 0.111.0 binary downloaded from GitHub releases, SHA-256 verified, cached at build time |
| MCP client | `src/agent/nu_session.rs` | `NuSession`: lazy spawn, JSON-RPC 2.0 over stdio, per-phase sandbox via lot |
| Tool layer | `src/agent/tools.rs` | `tool_nu` delegates to `NuSession::evaluate`; `ToolGrant::NU` replaces `BASH` |
| Sandbox policy | `src/agent/nu_session.rs` | `build_nu_sandbox_policy` — same lot policy, now anchored to the persistent process |
| Flick integration | `src/agent/flick.rs` | `ToolExecutor::execute` takes `&NuSession`; one session per `run_with_tools` call |

## NuShell Compatibility Notes

1. **Exit codes**: Standard (0 = success). MCP responses include exit status.
2. **Environment variables**: NuShell reads them. Lot's `forward_common_env()` handles environment filtering at process spawn.
3. **Process signals**: NuShell responds to SIGKILL / TerminateProcess normally.
4. **Syntax**: LLM agents generate NuShell syntax instead of POSIX sh. The tool name and description handle this implicitly.

## Timeout Handling

On timeout, epic kills the nu MCP process entirely and returns an error to the agent indicating the session was terminated. The next tool call spawns a fresh nu session (session state is lost). The agent recovers naturally — the error message tells it what happened, and it proceeds with a clean session. No MCP-level request cancellation needed.

## References

- [NuShell GitHub](https://github.com/nushell/nushell) — Source, 37-crate workspace, version 0.111.0
- [NuShell 0.108.0 release](https://www.nushell.sh/blog/2025-10-15-nushell_v0_108_0.html) — Initial MCP
- [NuShell 0.110.0 release](https://www.nushell.sh/blog/2026-01-17-nushell_v0_110_0.html) — MCP default, state persistence
- [NuShell 0.111.0 release](https://www.nushell.sh/blog/2026-02-28-nushell_v0_111_0.html) — HTTP transport, request cancellation
- [NuShell MCP issue #15435](https://github.com/nushell/nushell/issues/15435) — Original MCP proposal
