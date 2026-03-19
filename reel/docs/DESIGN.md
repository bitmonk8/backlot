# Design

[README](../README.md) is the primary entry point. This document covers
implementation details, internal architecture, and design rationale.

---

## Project Structure

```
reel/                            (workspace root)
├── Cargo.toml                   (workspace config, shared lints/versions/profile)
├── reel/                        (library crate)
│   ├── Cargo.toml
│   ├── build.rs                 — Downloads nu 0.111.0 + rg 14.1.1, SHA-256 verify, caches in target/nu-cache/
│   ├── src/
│   │   ├── lib.rs               — Public API re-exports
│   │   ├── agent.rs             — Agent, AgentEnvironment, AgentRequestConfig, RunResult, ToolHandler trait, tool loop
│   │   ├── nu_session.rs        — NuSession: persistent nu --mcp process, JSON-RPC 2.0, sandbox lifecycle
│   │   ├── tools.rs             — ToolGrant bitflags, tool definitions, nu command translation, execute_tool dispatch
│   │   └── sandbox.rs           — Re-exports of lot's prerequisite APIs
├── reel-cli/                    (CLI binary crate)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs              — reel run, reel setup, config parsing, output formatting
├── docs/
├── prompts/
└── .github/
```

Library crate (`reel`) contains all agent logic. CLI binary (`reel-cli`) is a
thin wrapper for config parsing and output formatting.

---

## Dependencies

| Crate | Purpose |
|---|---|
| `flick` (git, rev `287bfbd`) | LLM client: RequestConfig, FlickClient, Context, ModelRegistry, ProviderRegistry |
| `lot` (git, rev `c3cc94d`) | Process sandboxing: SandboxPolicy, SandboxCommand, spawn, AppContainer/namespaces/Seatbelt |
| `tokio` | Async runtime (rt-multi-thread, macros, time, sync, process, fs, signal, io-util) |
| `anyhow` | Error handling |
| `serde` + `serde_json` | Serialization |
| `bitflags` | ToolGrant permission flags |
| `tempfile` | Per-session temp directories |

Build dependencies: `ureq` (HTTP download), `sha2` (checksum verify), `flate2` +
`tar` (Linux/macOS extraction), `zip` (Windows extraction).

---

## Agent Runtime (`agent.rs`)

### Core Types

```rust
pub struct AgentEnvironment {
    pub model_registry: flick::ModelRegistry,
    pub provider_registry: flick::ProviderRegistry,
    pub project_root: PathBuf,
    pub timeout: Duration,
}

pub struct AgentRequestConfig {
    pub config: flick::RequestConfig,
    pub grant: ToolGrant,
    pub custom_tools: Vec<Box<dyn ToolHandler>>,
}

pub struct RunResult<T> {
    pub output: T,
    pub usage: Option<Usage>,
    pub tool_calls: u32,
    pub response_hash: Option<String>,
}
```

`Agent` wraps `AgentEnvironment` (shared across calls). `AgentRequestConfig`
wraps a flick `RequestConfig` (reusable across calls). Query is per-invocation.

### Dispatch Heuristic

`Agent::run()` routes based on tool availability (built-in or custom):

- **Tools available** — tool-loop mode (`run_with_tools`): spawns NuSession, injects
  tool definitions, runs up to 50 rounds / 200 total tool calls.
- **No tools** — structured mode (`run_structured`): single flick call, no tools.

A consumer with only custom tools (no TOOLS grant) is correctly routed to tool-loop
mode — the heuristic checks `tool_definitions(grant)` and `custom_tools`, not the
grant flags directly.

### Tool Loop

1. `build_request_config` clones the flick `RequestConfig` and injects built-in
   + custom tool definitions.
2. `FlickClient::new()` resolves model to provider chain.
3. `client.run(query, &mut ctx)` — initial model call.
4. While response contains tool calls and rounds < 50 and total tool calls ≤ 200:
   a. For each tool call: dispatch to built-in handler (`execute_tool`) or custom
      `ToolHandler` by name match.
   b. Collect `ToolExecResult` for each call.
   c. `client.resume(&mut ctx, tool_results)` — continue conversation.
5. Extract final text/structured output from last response.
6. Timeout via `tokio::time::timeout` wrapping each `client.run()`/`client.resume()` call individually.

Custom tool dispatch uses a `HashMap<String, usize>` index built at dispatch time
for O(1) lookup. Custom tools are checked first via the HashMap (allows consumers
to override built-in tools), then built-in tools via the tool executor.

### Testability

Two injection seams:

- **`ClientFactory` trait** — mock flick client creation (avoids real provider calls).
- **`ToolExecutor` trait** — mock tool execution (avoids real nu/sandbox).

`Agent::with_injected()` (test-only) accepts both mocks and sets
`skip_nu_spawn = true`.

---

## NuShell Session (`nu_session.rs`)

### Architecture

`NuSession` manages a persistent `nu --mcp` child process. Communication is
JSON-RPC 2.0 over stdio (one JSON object per line).

Internal state:

- `NuProcess`: holds `SandboxedChild` via `ChildHandle`, stdin `File` via
  `StdinHandle`, stdout `BufReader`, grant, project root, and session temp dir.
- `SessionState` (behind `tokio::sync::Mutex`): holds `Option<NuProcess>`,
  generation counter, and shared `inflight_child`/`inflight_stdin` handles.
- The inflight handles enable out-of-band `kill()` — close stdin to trigger EOF
  even when the `NuProcess` has been taken out for blocking I/O in `evaluate_inner`.

### Process Lifecycle

**Spawn.** `NuSession::spawn()` builds a `SandboxPolicy` from grant flags and
project root, constructs a `SandboxCommand` for `nu --mcp`, and calls
`lot::spawn()`. Config files (`reel_config.nu`, `reel_env.nu`) generated by
`build.rs` are passed via `--config` and `--env-config`.

**Evaluate.** `evaluate()` → `evaluate_inner()`:

1. `ensure_and_take()`: ensures a compatible process exists and takes it out of
   state. Fast path checks and takes under one lock. Slow path releases the lock
   to spawn, re-acquires to install and take.
2. Send JSON-RPC `tools/call` request with tool name + arguments (blocking I/O
   on `spawn_blocking` thread — async mutex released during I/O).
3. Read response lines, skip non-matching IDs (up to 64 lines).
4. Parse MCP `ToolResult` → `NuOutput`.
5. Put process back (or discard if generation changed during evaluate).

**Kill.** `kill()` increments generation, closes stdin handle, kills child via
`SandboxedChild::kill()`.

**Drop.** `NuProcess::drop` calls `child.kill()`, then `bounded_reap()` polls
`try_wait()` for up to 5 seconds to reap the child (ensures handles are released
before temp dir cleanup on Windows). Abandons silently if the process does not
exit within the deadline. `bounded_reap` is a standalone testable function.

### Sandbox Policy

Built by `NuSession` from grant flags:

| Grant | Policy Effect |
|---|---|
| TOOLS only | project_root → read_paths, temp dir → write_paths |
| TOOLS + WRITE | project_root → write_paths, temp dir → write_paths |
| TOOLS + NETWORK | allow_network = true |
| Always | platform exec paths, platform lib paths, tool dir → exec_paths |

The lot `SandboxPolicyBuilder` handles auto-canonicalization, deduplication, and
platform defaults.

### Per-session Temp Directory

Each `NuSession` creates `<project_root>/.reel/tmp/<uuid>/` as the session temp
directory. This keeps the temp dir under the project root so that AppContainer
ancestor-traversal ACEs (granted by `reel setup`) cover the path on Windows.
The temp directory is added to write_paths and set as `TEMP`/`TMP` in the nu
environment. The `SessionTempDir` wrapper cleans up the per-session directory on
drop, and also removes the empty `.reel/tmp/` and `.reel/` parent directories
if no other session is using them.

### Grant-change Respawn

If `evaluate()` is called with different grant flags or project root than the
running process, the process is killed and respawned with updated sandbox policy.
A generation counter prevents stale processes from being reused.

### Tool Directory Resolution

The tool directory is resolved at runtime by `resolve_tool_dir()`:

1. **Exe-adjacent** — if `reel_config.nu` exists next to the current executable
   (same directory as the binary), that directory is used. This handles release
   packaging and binary relocation.
2. **Compile-time `NU_CACHE_DIR`** — the `build.rs`-emitted path, used during
   development when binaries and config live in `target/nu-cache/`.
3. **None** — no tool directory found; nu starts without custom commands.

---

## Tool System (`tools.rs`)

### ToolGrant Bitflags

```rust
bitflags! {
    pub struct ToolGrant: u8 {
        const WRITE   = 0b0000_0001;
        const TOOLS   = 0b0000_0010;
        const NETWORK = 0b0000_0100;
    }
}
```

`ToolGrant::from_names(&["write", "tools", "network"])` parses string names.
`"write"` and `"network"` imply `TOOLS` — callers need not specify `"tools"`
explicitly. Returns `GrantParseError` on unknown names.

### Tool Definitions

`tool_definitions(grant)` returns a `Vec<ToolDefinition>` based on grant flags:

- **TOOLS** — Read, Glob, Grep, NuShell
- **WRITE** (implies TOOLS) — adds Write, Edit

Each definition includes name, description, and JSON Schema parameters matching
the model's tool-calling format.

### Nu Command Translation

Built-in tools are translated to nu custom commands:

| Tool | Nu Command |
|---|---|
| `Read { path, offset?, limit? }` | `reel read '<path>' [--offset N] [--limit N]` |
| `Write { path, content }` | `reel write '<path>' '<content>'` |
| `Edit { path, old_string, new_string, replace_all? }` | `reel edit '<path>' '<old>' '<new>' [--replace-all]` |
| `Glob { pattern, path? }` | `reel glob '<pattern>' [--path '<path>']` |
| `Grep { pattern, path?, ... }` | `reel grep '<pattern>' [--path '<path>'] [--output-mode ...] [--glob ...] [--type ...] [--case-insensitive] [--context-before N] [--context-after N] [--context N] [--multiline] [--head-limit N] [--no-line-numbers]` (shells out to rg via `REEL_RG_PATH`) |
| `NuShell { command, timeout? }` | Direct evaluation of the command string |

The nu custom commands are defined in `reel_config.nu` (generated by `build.rs`).

### Output Handling

Tool output is truncated to 64 KiB (`MAX_NU_OUTPUT`). NuShell tool timeout
defaults to 120s, max 600s (model can request via `timeout` parameter). File
tools use a fixed 120s timeout.

`extract_text()` extracts the last text content block from the model response
(reverse iterator for efficiency).

---

## Sandbox Re-exports (`sandbox.rs`)

`reel::sandbox` re-exports lot's prerequisite APIs so library consumers do not
need a direct lot dependency:

- `is_elevated()` — whether current process is admin (Windows; always false elsewhere)
- `grant_appcontainer_prerequisites(paths)` — one-time ACL setup (Windows; no-op elsewhere)
- `appcontainer_prerequisites_met(paths)` — check if prerequisites exist (Windows; always true elsewhere)
- `grant_appcontainer_prerequisites_for_policy(policy)` — policy-based variant
- `appcontainer_prerequisites_met_for_policy(policy)` — policy-based variant
- `SandboxPolicy`, `SandboxError` — lot types

---

## Build System (`build.rs`)

Downloads and caches prebuilt binaries at compile time:

| Binary | Version | Purpose |
|---|---|---|
| NuShell | 0.111.0 | MCP server for tool execution |
| ripgrep | 14.1.1 | Backend for `reel grep` |

**Process:**

1. Determine target OS + arch from Cargo env vars.
2. Look up platform-specific asset (URL, SHA-256, binary name).
3. Check `target/nu-cache/` for existing binary — skip if present.
4. Download archive, verify SHA-256, extract binary.
5. Generate `reel_config.nu` and `reel_env.nu` for nu custom commands.
6. Emit `NU_CACHE_DIR` as compile-time env var for runtime path resolution.

Supported platforms: Windows (x86_64, aarch64), Linux (x86_64, aarch64), macOS
(x86_64, aarch64).

Skip env vars: `NU_SKIP_DOWNLOAD=1`, `RG_SKIP_DOWNLOAD=1`.

The runtime resolves `NU_CACHE_DIR` via `resolve_tool_dir()` — see NuShell Session section.

---

## CLI (`reel-cli/src/main.rs`)

Thin binary. All agent logic lives in the library crate.

### Config Parsing

YAML config parsing (deserialize-strip-reserialize):

1. Deserialize as `serde_yml::Value`.
2. Extract and remove `grant` key → `ToolGrant::from_names()`.
3. Re-serialize remainder → `flick::RequestConfig::from_str()`.

This handles flick's `deny_unknown_fields` constraint without duplicating its
deserialization.

### Output

- **Success** — compact JSON to stdout (`serde_json::to_string`).
- **Dry run** — compact JSON to stdout (`serde_json::to_string`), includes resolved grant names.
- **Error** — JSON to stdout with `status: "Error"`, exit code 1.
- **Human messages** — stderr only.

### Input Handling

- `--timeout 0` is rejected (`timeout must be at least 1 second`).
- Stdin read uses `tokio::task::spawn_blocking` to avoid blocking the async runtime.

---

## Testing Strategy

### Unit Tests

- `ToolGrant::from_names` parsing (valid, invalid, empty).
- Config parsing (CLI): valid grants, null/absent/invalid grant, unknown names,
  invalid YAML.
- Output serialization shapes (success, error).

### Integration Tests (require nu binary)

- Custom command execution: `reel read`, `reel write`, `reel edit`, `reel glob`,
  `reel grep`.
- Full `execute_tool()` path: Read, Write, Glob, NuShell, grant denial.
- Grant-change respawn (TOOLS→WRITE, TOOLS→NETWORK).
- Project-root-change respawn.
- Concurrent evaluate (both callers succeed).
- Kill during evaluate (process discarded, not written back).
- Network denial/allowance under sandbox.
- Agent tool loop with mock providers: timeout, custom tool dispatch, structured
  mode.

### Test Isolation

- `isolated_session()` helper creates a `NuSession` with a dedicated temp sandbox
  tool directory. Panics if `NU_CACHE_DIR` is not set at compile time, ensuring
  tests never silently fall back to an unsandboxed session.
- `sandbox_env()` wraps `isolated_session()` with an isolated project directory.
  These two functions are the required entry points for tests that need a
  `NuSession` -- direct use of `NuSession::new()` in tests bypasses isolation.
- Network sandbox tests use a local loopback `TcpListener` on an ephemeral port
  instead of external hosts, making denial/allowance verification deterministic
  regardless of internet connectivity. The allowed-network test uses
  `spawn_http_responder()` which spawns a background thread that accepts one
  connection and sends a minimal HTTP 200 response, ensuring `http get` succeeds
  and the sandbox-denial assertion on the `Ok` path is exercised.
- `looks_like_sandbox_denial(content)` is a shared helper that checks for
  sandbox denial keywords across platforms ("permission denied", "access denied",
  "operation not permitted", "not allowed", "seatbelt", "sandbox denial",
  "sandbox-exec", "appcontainer", "seccomp"). Both
  network tests use it, keeping the keyword list in one place.
- `skip_no_nu!()` macro skips integration tests when nu binary is unavailable.
- Mock `ClientFactory` and `ToolExecutor` for agent-level tests without real
  providers.

### CI

| Job | Platform | Notes |
|---|---|---|
| Format | All | `cargo fmt --all --check` |
| Clippy | Linux, macOS, Windows | `cargo clippy --workspace --all-targets` |
| Build | Linux, macOS, Windows | `cargo build --workspace` |
| Test (Linux) | ubuntu-latest | Parallel test execution, dynamic cgroup delegation |
| Test (macOS) | macos-latest | |
| Test (Windows) | windows-latest | |

Rust toolchain: 1.93.1. Dependencies pinned to git revs (lot `c3cc94d`, flick
`287bfbd`).
