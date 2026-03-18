# Known Issues

## Non-critical issues

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

### 13. `RunResult` field propagation untested

`reel/src/agent.rs` — `usage` and `response_hash` mapping from `FlickResult` is untested in both `run_structured` and `run_with_tools` paths. All mock providers use `UsageResponse::default()`. Need tests with non-default usage/hash values. **Category: Testing.**

### 14. `ToolCallsPending` in structured mode untested

`reel/src/agent.rs` — `run_structured` bails when the model hallucinates tool calls. No test covers this path. **Category: Testing.**

### 15. Multi-tool-call-per-round counting untested

`reel/src/agent.rs` — `total_tool_calls += tool_calls.len() as u32` accumulates across rounds. No test verifies correct counting when a single round returns multiple tool calls. **Category: Testing.**

### 17. reel-cli Windows setup functions duplicate cwd setup

`reel-cli/src/main.rs` — `check_windows_prerequisites` and `configure_windows_prerequisites` both do `current_dir()` → `vec![cwd.as_path()]`. Extract a helper. **Category: Simplification.**

### 18. `build_effective_config` is a trivial wrapper

`reel/src/agent.rs` — One-line public method that calls `Self::build_request_config(request)`. The indirection adds no value. **Category: Simplification.**

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

### 24. `MAX_TOOL_ROUNDS` caps rounds, not individual tool calls

`reel/src/agent.rs` — A model sending 5 tool calls per round can execute 250+ calls within 50 rounds. Consider adding a per-session tool call cap. **Category: Design.**

### 25. `WRITE` without `NU` is accepted but meaningless

`reel/src/agent.rs` — `ToolGrant::WRITE` without `ToolGrant::NU` produces empty tool list and routes to structured mode. No write capability. **Category: API clarity.**

### 26. Built-in file tools have fixed 120s timeout

`reel/src/tools.rs` — Only the NuShell tool respects model-provided timeout. File tools (Read, Write, Edit, Glob, Grep) use 120s. Slow Grep on large codebases cannot be extended. **Category: Feature gap.**

### 28. `reel glob` has no depth limit or symlink protection

`reel/build.rs` (REEL_CONFIG_NU) — `reel glob` runs `glob $pattern` with a 1000-result cap but no depth limit. A `**/*` pattern in a deep tree with symlink cycles could hang before the cap is reached. **Category: Robustness.**

### 29. `NuSession` leaves `.reel/tmp/` directory in project root

`reel/src/nu_session.rs` — Creates `<project_root>/.reel/tmp/` for tempdir. The tempdir itself is cleaned up, but the `.reel/tmp/` parent directory is left behind. Should use system temp dir or clean up the parent. **Category: Side effects.**

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

### 38. Grant-change respawn test does not cover NETWORK flag

`reel/src/nu_session.rs` — `integration_grant_change_respawns` only tests `NU` → `NU | WRITE`. No coverage for `NETWORK` flag change triggering respawn. Mechanism works via full bitflags comparison, so regression is unlikely. **Category: Testing.**

### 39. Network integration tests use external host (`httpbin.org`)

`reel/src/nu_session.rs` — `integration_sandbox_network_denied_without_grant` and `integration_sandbox_network_allowed_with_grant` hit `httpbin.org`. If the host is unreachable, the denial test passes for the wrong reason (cannot distinguish sandbox block from network unavailability) and the allowed test becomes a no-op. Fix: use a local loopback listener instead. **Category: Testing.**

### 40. Missing Edit and Grep end-to-end tests via `execute_tool()`

`reel/src/nu_session.rs` — Issue #2 integration tests cover Read, Write, Glob, NuShell, and grant denial through `execute_tool()`. Edit and Grep are tested only at the nu custom-command level, not through the full `execute_tool()` → `translate_tool_call` → `format_tool_result` path. **Category: Testing.**

### 41. `ToolGrant::from_names` does not test empty string element

`reel/src/tools.rs` — `from_names(&[""])` (empty string element) is not tested. Depending on the implementation, an empty string could be treated as unknown or cause unexpected behavior. **Category: Testing.**

### 42. `eprintln!` in library `NuProcess::drop`

`reel/src/nu_session.rs` — `NuProcess::drop` uses `eprintln!` to warn when the bounded wait times out. Library crates should not write directly to stderr; consumers embedding reel get unexpected output. Replace with `tracing::warn!` when a logging dependency is added, or remove the warning. **Category: Separation of concerns.**

### 43. `NuProcess::drop` timeout branch untested

`reel/src/nu_session.rs` — The 5-second deadline branch in `NuProcess::drop` (where `try_wait` never returns exit status) has no test coverage. The poll loop is embedded in `Drop` on a concrete type with no test seam. Extract into a testable function to enable unit testing. **Category: Testing.**

### 44. TOCTOU re-check path in `evaluate_inner` untested

`reel/src/nu_session.rs` — The `still_needs == false` branch (where a concurrent caller already installed a compatible process) in `ensure_process`'s respawn block has no test coverage. Core correctness invariant of the lock-free spawn pattern. **Category: Testing.**

### 45. `spawn()` respawn on parameter mismatch untested

`reel/src/nu_session.rs` — `spawn()` called with different project_root or grant (triggering kill-and-respawn) has no direct test. Covered indirectly via `evaluate` path. **Category: Testing.**

### 46. `evaluate_inner` Phase 3 generation-mismatch-with-OK-result untested

`reel/src/nu_session.rs` — When `evaluate_inner` completes successfully but `generation != generation_at_start` (a concurrent `kill()` or respawn occurred), the process is silently dropped instead of written back. This discard path has no test coverage. **Category: Testing.**

### 47. `evaluate_inner` process steal race after `ensure_process`

`reel/src/nu_session.rs` — `evaluate_inner` calls `ensure_process()` (which releases and re-acquires the lock internally), then acquires the lock again to `.take()` the process. Between these two lock acquisitions, a concurrent caller could `.take()` the process, causing `"internal: process unavailable after spawn"`. Not triggered today (evaluate calls are sequential per agent turn), but latent if concurrent evaluate is ever used. **Category: Concurrency.**
