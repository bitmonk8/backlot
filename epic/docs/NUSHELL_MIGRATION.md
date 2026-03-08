# NuShell Migration Spec

Replace POSIX `sh` with [NuShell](https://www.nushell.sh/) as epic's sole shell runtime.

## Motivation

POSIX `sh` is unix-centric. On Windows it requires Git Bash or similar, and behavior diverges across platforms. NuShell is cross-platform with consistent behavior on Windows, Linux, and macOS.

## NuShell Background

NuShell is open source, written in Rust (repo: https://github.com/nushell/nushell). It publishes Rust crates that allow building custom NuShell binaries with additional features enabled.

The core NuShell project includes built-in MCP server functionality that can be enabled via command-line arguments in a custom build. Third-party NuShell MCP servers exist but are not relevant — only the core project's built-in MCP capability matters.

## Integration Strategy

Epic spawns NuShell as an external process — same pattern as the current `sh` invocation. No in-process embedding.

### Phase 1: Swap shell binary

Epic spawns `nu -c <command>` as a child process instead of `sh -c <command>`.

- Minimal code changes (rename + swap binary).
- Sandboxing via lot works as-is (lot sandboxes the spawned process).
- Requires `nu` on PATH.

### Phase 2: Custom NuShell build as Cargo dependency

Build a custom `nu` binary from NuShell's Rust crates with MCP server support enabled (activated via command-line arguments). Epic would depend on the NuShell crates at the build level to produce this custom binary, but NuShell still runs out-of-process.

- Epic ships its own `nu` binary — no separate install required.
- MCP server mode: epic spawns `nu --mcp` (or similar) once, keeps it running, communicates via MCP protocol over stdio.
- **Persistent session**: Multiple tool calls reuse the same NuShell instance — REPL-style, similar to how Claude Code maintains a persistent bash session. Environment variables, working directory, and shell state carry across calls. Current approach spawns a fresh `sh` per tool call, losing all state each time.
- Sandboxing via lot still applies — the custom `nu` binary is still a spawned child process.
- Larger build, but no fundamental changes to epic's process model (still out-of-process).

## Current State

The shell tool is implemented in `src/agent/tools.rs`. Key facts:

- Tool name: `"bash"`
- Shell binary: `"sh"` invoked with `-c <command>` (lines 649, 714)
- Two execution paths: sandboxed (via lot `SandboxCommand`) and unsandboxed fallback (via `tokio::process::Command`)
- Grant flag: `ToolGrant::BASH` (bitflag `0b0000_0100`)
- Available in Execute and Decompose phases; not in Analyze
- Constants: `MAX_BASH_OUTPUT` (64 KiB), `DEFAULT_BASH_TIMEOUT_SECS` (120), `MAX_BASH_TIMEOUT_SECS` (600)
- Internal types: `BashOutput` struct, functions `tool_bash`, `tool_bash_sandboxed`, `tool_bash_unsandboxed`, `format_bash_output`, `bash_output_from`
- Process group management: `setsid()` on Unix, `CREATE_NEW_PROCESS_GROUP` on Windows — shell-agnostic
- Environment: `env_clear()` + whitelist of safe vars (`UNSANDBOXED_ENV_KEYS`)
- Tests: ~15 bash-specific tests (lines 1192–1419)

## Required Changes

### Source code

#### `src/agent/tools.rs`

| Item | Current | New |
|---|---|---|
| Tool name (line 135) | `"bash"` | `"nu"` |
| Tool description (line 136) | `"Execute a bash command..."` | `"Execute a NuShell command..."` |
| Shell binary (lines 649, 714) | `"sh"` | `"nu"` |
| Shell args (lines 650, 715) | `["-c", &command]` | `["-c", &command]` (unchanged) |
| Constants | `MAX_BASH_OUTPUT`, `DEFAULT_BASH_TIMEOUT_SECS`, `MAX_BASH_TIMEOUT_SECS` | `MAX_NU_OUTPUT`, `DEFAULT_NU_TIMEOUT_SECS`, `MAX_NU_TIMEOUT_SECS` |
| Struct | `BashOutput` | `NuOutput` |
| Functions | `tool_bash`, `tool_bash_sandboxed`, `tool_bash_unsandboxed`, `format_bash_output`, `bash_output_from` | `tool_nu`, `tool_nu_sandboxed`, `tool_nu_unsandboxed`, `format_nu_output`, `nu_output_from` |
| Grant flag (line 28) | `ToolGrant::BASH` | `ToolGrant::NU` |
| Grant check (line 248) | `"bash" => BASH` | `"nu" => NU` |
| Phase grants (lines 48–50) | `ToolGrant::BASH` | `ToolGrant::NU` |
| Error messages | `"tool 'bash' not permitted..."` | `"tool 'nu' not permitted..."` |

Tests: rename functions, update command syntax where it differs (e.g. `sleep 10` → `sleep 10sec`).

#### `src/agent/config_gen.rs`

No changes expected — tool list is generated dynamically via `tool_definitions(grant)`.

#### `src/agent/prompts.rs`

No changes expected — prompts reference tools generically ("run commands as needed"). The tool name and description in the tool definition tell the model which shell to use.

#### `src/config/project.rs`, `src/init.rs`

No changes — `VerificationStep.command` is a `Vec<String>` (program + args), not a shell string.

### Unchanged subsystems

- **Process management**: `setsid` / `CREATE_NEW_PROCESS_GROUP` / `kill_process_tree` — operates on spawned process regardless of shell.
- **Sandbox policy**: lot `SandboxPolicy` is shell-agnostic.
- **Output handling**: exit code convention (0 = success) and stdout/stderr capture work the same.
- **Environment scrubbing**: `env_clear()` + whitelist works with NuShell.

### Documentation

| File | Change |
|---|---|
| `docs/STATUS.md` | `bash` → `nu` in tools list and sandboxing description |
| `docs/SANDBOXING.md` | "bash tool" / "bash commands" → "nu tool" / "nu commands" |
| `docs/CONFIGURATION.md` | Update any bash references |
| `docs/LOT_SPEC.md` | Update if `sh` appears as example program |
| `docs/audit/X6.md` | `DEFAULT_BASH_TIMEOUT_SECS` → `DEFAULT_NU_TIMEOUT_SECS` |
| `docs/audit/U5-R2.md` | "bash tool" → "nu tool" |
| `docs/audit/U1-R2.md` | "bash tool" → "nu tool" |
| `README.md` | Tool table: `bash` → `nu` |
| `docs/OVERVIEW.md` | Update document index if needed |

### Not in scope (Phase 1)

- Making the shell configurable — this is a hard switch, not an option.
- Changing `VerificationStep.command` format — already shell-agnostic.
- Lot library changes — lot spawns whatever program it's given.
- Custom NuShell build / MCP server integration (Phase 2).

## NuShell Compatibility Notes

1. **`-c` flag**: NuShell supports `nu -c "command"` — same invocation pattern as `sh -c`.
2. **Exit codes**: Standard (0 = success). Output formatting logic unchanged.
3. **stdin=Null**: Works — no interactive mode when given `-c`.
4. **Environment variables**: NuShell reads them. The `env_clear()` + whitelist approach works.
5. **Process signals**: NuShell responds to SIGKILL / TerminateProcess normally.
6. **Syntax**: LLM agents generate NuShell syntax instead of POSIX sh. The tool name and description handle this implicitly.

## Open Questions

1. **Startup check for `nu` binary** (Phase 1): Should epic verify `nu` is on PATH at startup, or let it fail on first tool invocation? A startup check gives a better error message. Recommendation: add a check during CLI init / `FlickAgent::new()`.

2. **Custom build specifics** (Phase 2): Which NuShell crates are needed to build a custom `nu` binary with MCP server support? What command-line arguments enable MCP mode? Needs investigation of the NuShell source.

3. **MCP integration with Flick** (Phase 2): If NuShell runs as a persistent MCP server, how does this interact with epic's Flick-based agent architecture? Could epic's tool dispatch route through NuShell's MCP interface instead of `nu -c`? Needs design work.

4. **Session lifecycle** (Phase 2): When is the persistent NuShell instance spawned and torn down? Per task? Per epic run? How does session state (env vars, cwd) interact with sandboxing policy changes between phases (read-only in Decompose vs writable in Execute)?
