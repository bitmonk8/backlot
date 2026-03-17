# Known Issues

## Non-critical issues

### 1. No tests for `ToolHandler` dispatch

`reel/src/agent.rs` — No tests cover the custom tool dispatch path: name matching against model tool calls, priority over built-ins, unknown tool error handling. Add when implementing first consumer (epic's Research Service). **Category: Testing.**

### 2. No integration tests for full tool execution path

`reel/src/tools.rs`, `reel/src/nu_session.rs` — Unit tests exist for `quote_nu()`, grant checks, sandbox policy, and response parsing. No end-to-end test validates `execute_tool()` → NuSession command translation → subprocess execution → result parsing. **Category: Testing.**

### 3a. `NuProcess::drop` uses unbounded `wait()` after `kill()`

`child.wait()` in `Drop` uses `WaitForSingleObject(INFINITE)`. If `kill()` fails silently, `wait()` blocks indefinitely. Practical likelihood very low. Drop impl relies on struct field declaration order for kill→wait→TempDir-drop. **Category: Correctness (edge case).**

### 3b. Per-session temp dir test gaps

Missing tests: nu seeing overridden `TEMP`/`TMP` env vars, read-only session writing to temp dir, temp dir cleanup on drop, `spawn_nu_process` with nonexistent `project_root`, policy test asserting absence of system temp dirs from `write_paths`. **Category: Testing.**

### 3c. Policy test boilerplate duplication

5 `build_nu_sandbox_policy` tests repeat identical `TempDir` + `TempDir::new_in` setup. Extract a helper. **Category: Simplification.**

### 3d. Tests assume `REEL_RG_PATH` is always set

Two tests use `^$env.REEL_RG_PATH` without guarding for absence. **Category: Testing.**

### 3e. No test for `rg_binary = None` branch

No test covers `resolve_rg_binary` returning `None`. **Category: Testing.**

### 3f. `resolve_rg_binary` has no direct unit tests

Tested only indirectly through integration tests. **Category: Testing.**

### 3g. `isolated_session()` silent fallback defeats isolation

Falls back to `NuSession::new()` when `tmp_sandbox_cache()` returns `None`. Should panic instead. **Category: Testing.**

### 3h. No mechanism to prevent `NuSession::new()` in tests

Nothing prevents tests from using `NuSession::new()` directly instead of `isolated_session()`. **Category: Testing.**

### 5. `Agent::run()` dispatch heuristic uses `ToolGrant::NU` instead of tool availability

`reel/src/agent.rs` — `run()` decides between structured and tool-loop mode based on `ToolGrant::NU`. A consumer with only custom tools (no NU grant) would be routed to structured mode incorrectly. No such consumer exists yet. **Category: Correctness.**

### 6. No test for timeout during resume (tool loop) phase

`reel/src/agent.rs` — Timeout during `client.resume()` in the tool loop is not tested. The structured-mode timeout test covers the pattern. **Category: Testing.**

### 7. No test for project root change triggering NuSession respawn

`reel/src/nu_session.rs` — `evaluate_inner` restarts the nu process when project root changes. Grant-change respawn test covers the mechanism but not this specific trigger. **Category: Testing.**

### 8. `NuSession` re-exported but no external consumer

`reel/src/lib.rs` — `pub use nu_session::NuSession` is part of the public API but no consumer uses it directly. Consider removing after API stabilization. **Category: Placement.**

### 10. `extract_text` uses mutable loop instead of iterator

`reel/src/agent.rs` — `extract_text` iterates forward with a `let mut last_text` variable. Replace with `result.content.iter().rev().find_map(...)`. **Category: Simplification.**

### 11. `dispatch_tool` linear scan on custom tools

`reel/src/agent.rs` — `dispatch_tool` calls `handler.definition()` on every custom tool handler for every tool call, just to compare names. Build a `HashMap<&str, usize>` once at the start of `run_with_tools`. Low urgency — custom tool count will be 0–1 near term. **Category: Simplification.**

### 12. `parse_config` and `emit_error` untested in reel-cli

`reel-cli/src/main.rs` — `parse_config` (single-pass YAML: parse as Value, pop `grant`, re-serialize remainder to flick) has no test coverage. Multiple branches: valid grant sequence, null/absent grant, non-sequence grant error, non-string element error, `from_names` error propagation. `emit_error` output shape is part of the CLI's documented interface. **Category: Testing.**

### 13. `RunResult` field propagation untested

`reel/src/agent.rs` — `usage` and `response_hash` mapping from `FlickResult` is untested in both `run_structured` and `run_with_tools` paths. All mock providers use `UsageResponse::default()`. Need tests with non-default usage/hash values. **Category: Testing.**

### 14. `ToolCallsPending` in structured mode untested

`reel/src/agent.rs` — `run_structured` bails when the model hallucinates tool calls. No test covers this path. **Category: Testing.**

### 15. Multi-tool-call-per-round counting untested

`reel/src/agent.rs` — `total_tool_calls += tool_calls.len() as u32` accumulates across rounds. No test verifies correct counting when a single round returns multiple tool calls. **Category: Testing.**

### 16. ~~reel-cli two-pass YAML config parse over-engineered~~ RESOLVED

Resolved: single-pass parse as `serde_yml::Value`, pop `grant`, pass remainder to flick.

### 17. reel-cli Windows setup functions duplicate cwd setup

`reel-cli/src/main.rs` — `check_windows_prerequisites` and `configure_windows_prerequisites` both do `current_dir()` → `vec![cwd.as_path()]`. Extract a helper. **Category: Simplification.**

### 18. `build_effective_config` is a trivial wrapper

`reel/src/agent.rs` — One-line public method that calls `Self::build_request_config(request)`. The indirection adds no value. **Category: Simplification.**

### 20. `evaluate_inner` holds lock during nu process spawn

`reel/src/nu_session.rs` — `evaluate_inner` awaits `spawn_nu_process` while holding the async `Mutex`, blocking `kill()` if spawn hangs. Should spawn outside the lock. **Category: Concurrency.**

### 21. `spawn()` does not verify grant/project_root match

`reel/src/nu_session.rs` — `spawn()` returns early if `process.is_some()` without checking whether the existing process matches the requested parameters. **Category: Correctness.**

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

### 24. `MAX_TOOL_ROUNDS` caps rounds, not individual tool calls

`reel/src/agent.rs` — A model sending 5 tool calls per round can execute 250+ calls within 50 rounds. Consider adding a per-session tool call cap. **Category: Design.**

### 25. `WRITE` without `NU` is accepted but meaningless

`reel/src/agent.rs` — `ToolGrant::WRITE` without `ToolGrant::NU` produces empty tool list and routes to structured mode. No write capability. **Category: API clarity.**

### 26. Built-in file tools have fixed 120s timeout

`reel/src/tools.rs` — Only the NuShell tool respects model-provided timeout. File tools (Read, Write, Edit, Glob, Grep) use 120s. Slow Grep on large codebases cannot be extended. **Category: Feature gap.**

### 27. ~~`build_request_config` reconstructs config via accessors — may lose future fields~~ RESOLVED

Resolved: `RequestConfig` now derives `Clone`; `build_request_config` clones and mutates via `add_tools`.

### 28. `reel glob` has no depth limit or symlink protection

`reel/build.rs` (REEL_CONFIG_NU) — `reel glob` runs `glob $pattern` with a 1000-result cap but no depth limit. A `**/*` pattern in a deep tree with symlink cycles could hang before the cap is reached. **Category: Robustness.**

### 29. `NuSession` leaves `.reel/tmp/` directory in project root

`reel/src/nu_session.rs` — Creates `<project_root>/.reel/tmp/` for tempdir. The tempdir itself is cleaned up, but the `.reel/tmp/` parent directory is left behind. Should use system temp dir or clean up the parent. **Category: Side effects.**

### 30. ~~`ToolGrant::from_names` returns `Result<_, String>` instead of a proper error type~~ RESOLVED

Resolved: `GrantParseError` struct defined in `tools.rs`, re-exported from `reel::GrantParseError`.

### 31. `test_support` re-exports leak flick internal types through reel's public API

`reel/src/lib.rs` — When `testing` feature is enabled, types like `SingleShotProvider`, `DynProvider`, etc. are re-exported from flick. Changes to flick's test types break reel's API without any reel code changing. **Category: Semver hazard.**

### 32. `NU_CACHE_DIR` is baked in at compile time

`reel/src/nu_session.rs` — `option_env!("NU_CACHE_DIR")` is resolved at build time. If the binary is relocated, config files path goes stale and `resolve_config_files()` returns `None`, causing nu to start without custom commands (tool calls fail). Binary fallback works but config does not. **Category: Portability.**

### 33. reel-cli blocking stdin read on single-threaded async runtime

`reel-cli/src/main.rs` — `std::io::stdin().read_to_string()` blocks the `current_thread` tokio runtime. Works in practice since it runs before any async work, but should use `spawn_blocking` or async IO. **Category: Correctness.**

### 34. reel-cli `--timeout 0` accepted without validation

`reel-cli/src/main.rs` — No lower bound on timeout value. `Duration::from_secs(0)` causes instant timeout on every operation. **Category: Validation.**

### 35. reel-cli dry run output omits grant info and uses different JSON format

`reel-cli/src/main.rs` — Dry run uses `to_string_pretty` and omits the resolved `ToolGrant`; success output uses `to_string` (compact). Inconsistent format and missing diagnostic info. **Category: Usability.**

### 36. `ToolGrant::from_names` has no unit tests

`reel/src/tools.rs` — No tests for `from_names` parsing of `"write"`, `"nu"`, `"network"`, combined flags, or unknown grant error. **Category: Testing.**

### 37. No integration test for sandbox network denial

`reel/src/nu_session.rs` — Unit tests check the `allow_network` policy field, but no integration test confirms the sandbox actually blocks network operations without `NETWORK` grant. Same gap pattern as write-path enforcement. **Category: Testing.**

### 38. Grant-change respawn test does not cover NETWORK flag

`reel/src/nu_session.rs` — `integration_grant_change_respawns` only tests `NU` → `NU | WRITE`. No coverage for `NETWORK` flag change triggering respawn. Mechanism works via full bitflags comparison, so regression is unlikely. **Category: Testing.**
