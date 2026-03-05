# Flick Library Migration Spec

**Status: Implemented.** All three phases complete. See [STATUS.md](STATUS.md) for details.

## Summary

Replace Flick subprocess invocation with direct library calls. Flick exposes a Rust library crate (`flick`) alongside its CLI binary. Epic will depend on the library, eliminating process spawning, YAML config file I/O, and tool result file I/O.

## Motivation

- **No process overhead.** Eliminates spawn/wait/kill-on-drop per agent call.
- **No config file I/O.** Config structs built in-memory instead of writing YAML to disk.
- **No tool result file I/O.** Tool results passed as `Vec<ContentBlock>` instead of JSON files.
- **Stronger typing.** Shared Rust types (`FlickResult`, `ContentBlock`, `Config`) instead of parsing JSON from stdout.
- **Simpler error handling.** `FlickError` instead of exit code / stderr parsing.
- **No `--flick-path` configuration.** Users no longer need Flick on `$PATH` or configured via CLI flag.

## Dependency

```toml
[dependencies]
flick = { git = "https://github.com/bitmonk8/flick" }
```

No `cli` feature needed — Epic only uses the library API.

### Dependency impact

Flick's transitive dependencies: serde, serde_json, serde_yml, toml, reqwest (rustls-tls), tokio, thiserror, chacha20poly1305, zeroize, hex, xxhash-rust. Most overlap with Epic's existing dependencies (serde, serde_json, toml, tokio, thiserror). Net new: reqwest, serde_yml, chacha20poly1305, zeroize, hex, xxhash-rust.

## Architecture

### Current flow (subprocess)

```
Epic                                     Flick process
 |                                            |
 |-- write YAML config to disk -------------->|
 |-- spawn flick run --config ... --query ... |
 |                                            |-- call Anthropic API
 |                                            |-- return JSON on stdout
 |<-- parse FlickOutput from stdout ----------|
 |                                            |
 |   (if tool_calls_pending)                  |
 |-- execute tools locally                    |
 |-- write tool results JSON to disk -------->|
 |-- spawn flick run --config ... --resume .. |
 |                                            |-- call Anthropic API
 |<-- parse FlickOutput from stdout ----------|
 |   (repeat until complete)                  |
```

### New flow (library)

```
Epic
 |
 |-- Config::parse_yaml(yaml_str) or build Config in-memory
 |-- resolve_provider(&config).await
 |-- FlickClient::new(config, provider)
 |-- client.run(query, &mut context).await
 |
 |   (if ToolCallsPending)
 |-- execute tools locally
 |-- build Vec<ContentBlock::ToolResult>
 |-- client.resume(&mut context, tool_results).await
 |   (repeat until Complete)
 |
 |-- extract text from FlickResult.content
```

## Changes by File

### Cargo.toml

- Add `flick` git dependency.
- Remove `serde_yaml` (was only used for Flick config serialization; Flick library handles its own config parsing). If `serde_yaml` is still used elsewhere, keep it.

### src/agent/flick.rs — Major rewrite

**Remove:**
- `FlickOutput`, `ContentBlock`, `UsageSummary`, `FlickError` structs (replaced by `flick::FlickResult`, `flick::ContentBlock`, etc.)
- `ToolResultEntry` struct (replaced by `flick::ContentBlock::ToolResult`)
- `invoke_flick()` — subprocess spawning
- `format_exit_status()` helper
- All subprocess-related error handling (exit codes, stderr parsing, kill_on_drop)

**Replace with:**
- `FlickAgent` fields: replace `flick_path: PathBuf` with pre-resolved credential store or credential name. Remove `work_dir` (no config/result files to write). Keep `project_root`, `call_timeout`.
- `FlickAgent::new()` — construct `CredentialStore` (or accept one), validate credential exists.
- New private method: `build_client(&self, config: Config) -> Result<FlickClient>` — calls `resolve_provider`, constructs `FlickClient`.
- `run_structured()` — builds Config in-memory, creates `FlickClient`, calls `client.run()`, extracts text from `FlickResult.content`.
- `run_with_tools()` — same but loops on `ResultStatus::ToolCallsPending`, executes tools, builds `ContentBlock::ToolResult` entries, calls `client.resume()`.

**Keep unchanged:**
- `AgentService` impl methods (assess, execute_leaf, etc.) — they call `run_structured` / `run_with_tools` which change internally.
- `extract_text()` — adapt to use `flick::ContentBlock` instead of local enum.
- `extract_tool_calls()` — adapt to use `flick::ContentBlock::ToolUse`.
- `explore_for_init()` — same pattern, just uses library instead of subprocess.

### src/agent/config_gen.rs — Simplify

**Remove:**
- `FlickConfig`, `FlickModelConfig`, `FlickProviderConfig` structs (replaced by `flick::Config`).
- `write_config()` async function (no files to write).
- `serde_yaml` usage.

**Replace with:**
- Config builder functions that return `flick::Config` directly. Two options:
  1. Build YAML string in-memory, parse via `Config::parse_yaml()`. Minimal code change — reuse existing YAML structure.
  2. Build `Config` struct directly if Flick exposes a builder. Depends on Flick's public API.

Option 1 is recommended (least disruption). The `build_*_config` functions would return `flick::Config` instead of `FlickConfig`.

**Keep unchanged:**
- Wire format types (`AssessmentWire`, `DecompositionWire`, etc.) and their `TryFrom` impls.
- Output schema generators.
- `parse_model_name()`.

### src/agent/tools.rs — Minor change

- `FlickToolDef` struct: check if Flick's `Config` tool format matches. If Flick uses a different tool definition structure in its config, adapt `FlickToolDef` to match or remove it in favor of Flick's type.
- `tool_definitions()` return type may change.

### src/agent/mod.rs — No change

`AgentService` trait is unchanged.

### src/agent/models.rs — No change

Model ID mapping stays the same (used in config generation).

### src/agent/prompts.rs — No change

Prompt assembly is independent of transport.

### src/cli.rs — Simplify

- Remove `--flick-path` global option and `EPIC_FLICK_PATH` env var.
- `--credential` / `EPIC_CREDENTIAL` remains (passed to Flick's credential store).

### src/main.rs — Simplify

- Remove `flick_path` wiring.
- `FlickAgent::new()` no longer needs `flick_path` or `work_dir`.

### src/init.rs — Minor

- Follows same pattern change as other agent calls. No structural change.

## Migration Strategy

### Phase 1: Add dependency, implement new FlickAgent internals

1. Add `flick` to Cargo.toml.
2. Rewrite `FlickAgent` internals to use `FlickClient`.
3. Adapt `config_gen.rs` to produce `flick::Config`.
4. Map `flick::ContentBlock` to/from Epic's tool execution.

### Phase 2: Remove subprocess code

1. Remove `invoke_flick()`, `FlickOutput`, local `ContentBlock` enum, `ToolResultEntry`.
2. Remove `--flick-path` CLI option.
3. Remove `work_dir` from `FlickAgent` (no files to write).
4. Remove `format_exit_status()`.

### Phase 3: Clean up

1. Remove `serde_yaml` dependency if no longer used elsewhere.
2. Update `FLICK_INTEGRATION.md` to reflect library mode.
3. Update tests — adapt existing unit tests from local types to `flick::` types.

## Type Mapping

| Epic (current) | Flick library |
|---|---|
| `FlickOutput` (local) | `flick::FlickResult` |
| `ContentBlock` (local enum) | `flick::ContentBlock` |
| `UsageSummary` (local) | `flick::FlickResult.usage` |
| `FlickError` (local) | `flick::FlickError` |
| `ToolResultEntry` (local) | `flick::ContentBlock::ToolResult` |
| `FlickConfig` (local) | `flick::Config` |
| stdout JSON string | `FlickResult` (direct Rust value) |
| exit code + stderr | `Result<FlickResult, FlickError>` |
| `context_hash` field | `flick::Context` (managed by caller) |

## Context Management

Key difference: with the subprocess, each invocation was stateless — context was tracked via `context_hash` and `--resume` flag. With the library, `Context` is an in-memory struct owned by Epic. The tool loop becomes:

```rust
let mut context = Context::default();
let result = client.run(query, &mut context).await?;

while result.status == ResultStatus::ToolCallsPending {
    let tool_results = execute_tools(&result.content, &project_root, grant).await;
    let result = client.resume(&mut context, tool_results).await?;
}
```

No context hash needed. No `--resume` flag. The `Context` accumulates the full conversation.

## Timeout Handling

Current: `tokio::time::timeout` wrapping `child.wait_with_output()`.

New: `tokio::time::timeout` wrapping `client.run()` / `client.resume()`. Same pattern. If timeout fires, the HTTP request inside Flick is dropped (reqwest future cancelled), which is cleaner than process kill.

## Error Handling

Current: Parse exit code, try to extract structured error from stdout, fall back to stderr.

New: `FlickError` enum with `.code()` method. Map to `anyhow::Error` at the call site. Simpler and more reliable.

## Impact on Tests

- 7 unit tests in `flick.rs` test local types (`FlickOutput`, `ContentBlock`, etc.). These will be removed or rewritten to test against `flick::FlickResult` / `flick::ContentBlock`.
- 12 tests in `config_gen.rs` — wire format tests are unchanged. Config serialization tests (`config_serializes_to_yaml`, `write_config_creates_file`) will be removed or rewritten.
- Integration tests (if any) that mock the Flick binary will need to mock at the `DynProvider` level instead.

## Risks

1. **Dependency weight.** Flick brings reqwest + rustls + crypto crates. This increases compile time and binary size. Mitigated by the fact that Flick was already a runtime dependency — now it's just linked rather than spawned.
2. **Version coupling.** Epic and Flick must use compatible Rust editions and dependency versions. Git dependency pins to a commit. Breaking changes in Flick's API require Epic updates.
3. **Credential store path.** Flick's `CredentialStore` defaults to `~/.flick/`. This is fine — same location the CLI uses.

## Not Changing

- `AgentService` trait — unchanged.
- Wire format types and `TryFrom` conversions — unchanged.
- Prompt assembly — unchanged.
- Tool execution (`tools.rs`) — unchanged (Epic still executes all tools).
- Output schemas — unchanged.
- Model ID mapping — unchanged.
- Orchestrator — unchanged (calls `AgentService` trait methods).
- TUI — unchanged.
- State persistence — unchanged.
