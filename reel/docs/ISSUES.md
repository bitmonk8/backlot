# Known Issues

Clusters ordered by impact/importance (highest first).

## 1. NuSession portability (#32)

Binary relocation breaks tool execution entirely — nu starts without custom commands and all tool calls fail.

### 32. `NU_CACHE_DIR` is baked in at compile time

`reel/src/nu_session.rs` — `option_env!("NU_CACHE_DIR")` is resolved at build time. If the binary is relocated, config files path goes stale and `resolve_config_files()` returns `None`, causing nu to start without custom commands (tool calls fail). Binary fallback works but config does not. **Category: Portability.**

## 2. NuSession temp dir and side effects (#3b, #29, #49)

#29 leaves artifacts in user project directories — a visible side effect. All concern temp directory creation, visibility, cleanup.

### 29. `NuSession` leaves `.reel/tmp/` directory in project root

`reel/src/nu_session.rs` — Creates `<project_root>/.reel/tmp/` for tempdir. The tempdir itself is cleaned up, but the `.reel/tmp/` parent directory is left behind. Should use system temp dir or clean up the parent. **Category: Side effects.**

### 3b. Per-session temp dir test gaps

Missing tests: nu seeing overridden `TEMP`/`TMP` env vars, read-only session writing to temp dir, temp dir cleanup on drop, `spawn_nu_process` with nonexistent `project_root`, policy test asserting absence of system temp dirs from `write_paths`. **Category: Testing.**

### 49. `policy_test_fixture` cache parameter has no `Some(...)` caller

`reel/src/nu_session.rs` — `policy_test_fixture(grant, cache)` accepts `Option<&Path>` but all callers pass `None`. The `includes_cache_dir_exec` test still constructs dirs manually because it needs a cache dir outside the project root. The parameter is untested. **Category: Testing.**

## 3. NuSession stderr and debuggability (#23)

Lost errors make debugging hard for all consumers.

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

## 4. Network test reliability (#39)

Tests can pass for the wrong reason, masking real sandbox regressions.

### 39. Network integration tests use external host (`httpbin.org`)

`reel/src/nu_session.rs` — `integration_sandbox_network_denied_without_grant` and `integration_sandbox_network_allowed_with_grant` hit `httpbin.org`. If the host is unreachable, the denial test passes for the wrong reason (cannot distinguish sandbox block from network unavailability) and the allowed test becomes a no-op. Fix: use a local loopback listener instead. **Category: Testing.**

## 5. Test isolation infrastructure (#3g, #3h)

#3g is a bug that silently defeats test isolation — tests may appear isolated but actually run unsandboxed.

### 3g. `isolated_session()` silent fallback defeats isolation

Falls back to `NuSession::new()` when `tmp_sandbox_cache()` returns `None`. Should panic instead. **Category: Testing.**

### 3h. No mechanism to prevent `NuSession::new()` in tests

Nothing prevents tests from using `NuSession::new()` directly instead of `isolated_session()`. **Category: Testing.**

## 6. Public API surface (#8, #31)

#31 is a semver hazard — flick internal type changes silently break reel's public API. Matters when external consumers exist.

### 31. `test_support` re-exports leak flick internal types through reel's public API

`reel/src/lib.rs` — When `testing` feature is enabled, types like `SingleShotProvider`, `DynProvider`, etc. are re-exported from flick. Changes to flick's test types break reel's API without any reel code changing. **Category: Semver hazard.**

### 8. `NuSession` re-exported but no external consumer

`reel/src/lib.rs` — `pub use nu_session::NuSession` is part of the public API but no consumer uses it directly. Consider removing after API stabilization. **Category: Placement.**

## 7. Agent run result and timeout tests (#6, #13, #53, #54)

Test gaps on `Agent::run()` return value propagation, timeout, and tool-call-cap boundary behavior.

### 53. No boundary test for `MAX_TOOL_CALLS` (exactly 200 succeeds)

`reel/src/agent.rs` — The cap check uses `> MAX_TOOL_CALLS` (strictly greater). No test verifies that exactly 200 tool calls succeeds. Would catch off-by-one if `>` changes to `>=`. **Category: Testing.**

### 54. Test name `run_with_tools_counts_multi_tool_rounds` is misleading

`reel/src/agent.rs` — The test verifies multi-call counting within a single round, not across multiple rounds. Rename to `run_with_tools_counts_multi_calls_in_round`. **Category: Naming.**

### 6. No test for timeout during resume (tool loop) phase

`reel/src/agent.rs` — Timeout during `client.resume()` in the tool loop is not tested. The structured-mode timeout test covers the pattern. **Category: Testing.**

### 13. `RunResult` field propagation untested

`reel/src/agent.rs` — `usage` and `response_hash` mapping from `FlickResult` is untested in both `run_structured` and `run_with_tools` paths. All mock providers use `UsageResponse::default()`. Need tests with non-default usage/hash values. **Category: Testing.**

## 8. Tool execution coverage (#40, #41, #26)

#26 is a feature gap (fixed 120s timeout). #40 and #41 are test gaps on the tool execution path.

### 26. Built-in file tools have fixed 120s timeout

`reel/src/tools.rs` — Only the NuShell tool respects model-provided timeout. File tools (Read, Write, Edit, Glob, Grep) use 120s. Slow Grep on large codebases cannot be extended. **Category: Feature gap.**

### 40. Missing Edit and Grep end-to-end tests via `execute_tool()`

`reel/src/nu_session.rs` — Issue #2 integration tests cover Read, Write, Glob, NuShell, and grant denial through `execute_tool()`. Edit and Grep are tested only at the nu custom-command level, not through the full `execute_tool()` → `translate_tool_call` → `format_tool_result` path. **Category: Testing.**

### 41. `ToolGrant::from_names` does not test empty string element

`reel/src/tools.rs` — `from_names(&[""])` (empty string element) is not tested. Depending on the implementation, an empty string could be treated as unknown or cause unexpected behavior. **Category: Testing.**

## 9. Nu glob robustness (#28)

Potential hang on pathological input with symlink cycles.

### 28. `reel glob` has no depth limit or symlink protection

`reel/build.rs` (REEL_CONFIG_NU) — `reel glob` runs `glob $pattern` with a 1000-result cap but no depth limit. A `**/*` pattern in a deep tree with symlink cycles could hang before the cap is reached. **Category: Robustness.**

## 10. reel-cli fixes (#33, #34, #35)

All in `reel-cli/src/main.rs`. #33 is a correctness issue (benign in practice). #34 and #35 are validation/usability.

### 33. reel-cli blocking stdin read on single-threaded async runtime

`reel-cli/src/main.rs` — `std::io::stdin().read_to_string()` blocks the `current_thread` tokio runtime. Works in practice since it runs before any async work, but should use `spawn_blocking` or async IO. **Category: Correctness.**

### 34. reel-cli `--timeout 0` accepted without validation

`reel-cli/src/main.rs` — No lower bound on timeout value. `Duration::from_secs(0)` causes instant timeout on every operation. **Category: Validation.**

### 35. reel-cli dry run output omits grant info and uses different JSON format

`reel-cli/src/main.rs` — Dry run uses `to_string_pretty` and omits the resolved `ToolGrant`; success output uses `to_string` (compact). Inconsistent format and missing diagnostic info. **Category: Usability.**

## 11. Ripgrep resolution tests (#3d, #3e, #3f)

Pure test gaps on a single code path (`resolve_rg_binary`).

### 3d. Tests assume `REEL_RG_PATH` is always set

Two tests use `^$env.REEL_RG_PATH` without guarding for absence. **Category: Testing.**

### 3e. No test for `rg_binary = None` branch

No test covers `resolve_rg_binary` returning `None`. **Category: Testing.**

### 3f. `resolve_rg_binary` has no direct unit tests

Tested only indirectly through integration tests. **Category: Testing.**

## 12. Custom tool dispatch (#48)

No practical impact — unsupported scenario, already guarded by `build_request_config`.

### 48. Duplicate custom tool names: HashMap changes first-match to last-match semantics

`reel/src/agent.rs` — `dispatch_tool` previously used linear scan (first match wins). The `HashMap<String, usize>` built via `collect()` keeps the last entry for duplicate keys. No practical impact: duplicate custom tool names are not a supported scenario, and `build_request_config` rejects duplicate names against built-ins. **Category: Testing.**

## 13. Grant model refinements (#51, #52)

### 51. `ToolGrant::READ` understates the flag's scope

`reel/src/tools.rs` — `READ` enables NuShell (arbitrary command execution) and the read-only built-in tools. The name suggests read-only access but actually enables command execution. A name like `TOOLS` would describe the scope better. **Category: Naming.**

### 52. `WRITE`/`NETWORK` → `READ` implication not enforced at type level

`reel/src/tools.rs` — The implication (WRITE implies READ, NETWORK implies READ) is enforced only in `from_names`. Library consumers constructing `ToolGrant` directly via bitflags can create bare `WRITE` without `READ`, which silently produces zero tool definitions. `tool_definitions` and `required_grant` defensively check `WRITE | READ` together, duplicating the invariant. Consider a normalizing constructor or custom `BitOr` to enforce at the type level. **Category: Separation of concerns.**

## 14. NuSession minor refinements (#55, #56, #57, #58, #59)

### 55. `ensure_and_take` inflight registration duplicated 3x

`reel/src/nu_session.rs` — The `st.inflight_child = Some(Arc::clone(...)); st.inflight_stdin = Some(Arc::clone(...));` pattern appears in three branches of `ensure_and_take` (fast path, generation-mismatch-with-compatible-process, install path). Extract a helper if the method grows further. **Category: Simplification.**

### 56. `ensure_and_take` slow path retry loop untested

`reel/src/nu_session.rs` — The `continue` branch in `ensure_and_take`'s slow path (generation changed, no compatible process available) has no dedicated test. Requires three concurrent actors to trigger deterministically. The generation mechanism guarantees correctness; the retry is defense-in-depth. **Category: Testing.**

### 57. Concurrent evaluate test does not verify two distinct processes

`reel/src/nu_session.rs` — `integration_concurrent_evaluate_both_succeed` asserts both evaluations succeed but does not verify that two distinct processes were used. Cannot distinguish sequential reuse from concurrent spawn without exposing internal state. **Category: Testing.**

### 58. No `bounded_reap` test for `Ok(Some(ExitStatus))` path

`reel/src/nu_session.rs` — All `bounded_reap` tests use `Err(...)` or `Ok(None)`. The `Ok(Some(status))` path (normal exit) is covered by the wildcard arm but has no dedicated test. **Category: Testing.**

### 59. `bounded_reap` test name `bounded_reap_returns_true_on_immediate_exit` misleading

`reel/src/nu_session.rs` — The test passes an `Err` to simulate exit, not an actual `Ok(Some(ExitStatus))`. Name implies a clean exit. **Category: Naming.**

### 60. Singleton `inflight_child`/`inflight_stdin` fields cannot track concurrent callers

`reel/src/nu_session.rs` — `SessionState` has single `inflight_child` and `inflight_stdin` fields. If two concurrent `evaluate` calls are in Phase 2 (blocking I/O), the second caller's `ensure_and_take` overwrites the first caller's handles. A `kill()` during this window only reaches the second caller's child; the first is unreachable. Not triggered today (agent turns are sequential; the concurrent test runs on single-threaded tokio). Would matter if true multi-threaded concurrent evaluate is ever supported. **Category: Correctness.**
