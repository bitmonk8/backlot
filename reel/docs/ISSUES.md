# Known Issues

Clusters ordered by impact/importance (highest first).

## 1. NuSession stderr and debuggability (#23)

Lost errors make debugging hard for all consumers.

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

## 2. Public API surface (#8, #31)

#31 is a semver hazard — flick internal type changes silently break reel's public API. Matters when external consumers exist.

### 31. `test_support` re-exports leak flick internal types through reel's public API

`reel/src/lib.rs` — When `testing` feature is enabled, types like `SingleShotProvider`, `DynProvider`, etc. are re-exported from flick. Changes to flick's test types break reel's API without any reel code changing. **Category: Semver hazard.**

### 8. `NuSession` re-exported but no external consumer

`reel/src/lib.rs` — `pub use nu_session::NuSession` is part of the public API but no consumer uses it directly. Consider removing after API stabilization. **Category: Placement.**

## 3. Agent run result and timeout tests (#6, #13, #53, #54)

Test gaps on `Agent::run()` return value propagation, timeout, and tool-call-cap boundary behavior.

### 53. No boundary test for `MAX_TOOL_CALLS` (exactly 200 succeeds)

`reel/src/agent.rs` — The cap check uses `> MAX_TOOL_CALLS` (strictly greater). No test verifies that exactly 200 tool calls succeeds. Would catch off-by-one if `>` changes to `>=`. **Category: Testing.**

### 54. Test name `run_with_tools_counts_multi_tool_rounds` is misleading

`reel/src/agent.rs` — The test verifies multi-call counting within a single round, not across multiple rounds. Rename to `run_with_tools_counts_multi_calls_in_round`. **Category: Naming.**

### 6. No test for timeout during resume (tool loop) phase

`reel/src/agent.rs` — Timeout during `client.resume()` in the tool loop is not tested. The structured-mode timeout test covers the pattern. **Category: Testing.**

### 13. `RunResult` field propagation untested

`reel/src/agent.rs` — `usage` and `response_hash` mapping from `FlickResult` is untested in both `run_structured` and `run_with_tools` paths. All mock providers use `UsageResponse::default()`. Need tests with non-default usage/hash values. **Category: Testing.**

## 4. Tool execution coverage (#40, #41, #26)

#26 is a feature gap (fixed 120s timeout). #40 and #41 are test gaps on the tool execution path.

### 26. Built-in file tools have fixed 120s timeout

`reel/src/tools.rs` — Only the NuShell tool respects model-provided timeout. File tools (Read, Write, Edit, Glob, Grep) use 120s. Slow Grep on large codebases cannot be extended. **Category: Feature gap.**

### 40. Missing Edit and Grep end-to-end tests via `execute_tool()`

`reel/src/nu_session.rs` — Issue #2 integration tests cover Read, Write, Glob, NuShell, and grant denial through `execute_tool()`. Edit and Grep are tested only at the nu custom-command level, not through the full `execute_tool()` → `translate_tool_call` → `format_tool_result` path. **Category: Testing.**

### 41. `ToolGrant::from_names` does not test empty string element

`reel/src/tools.rs` — `from_names(&[""])` (empty string element) is not tested. Depending on the implementation, an empty string could be treated as unknown or cause unexpected behavior. **Category: Testing.**

## 5. Nu glob robustness (#28)

Potential hang on pathological input with symlink cycles.

### 28. `reel glob` has no depth limit or symlink protection

`reel/build.rs` (REEL_CONFIG_NU) — `reel glob` runs `glob $pattern` with a 1000-result cap but no depth limit. A `**/*` pattern in a deep tree with symlink cycles could hang before the cap is reached. **Category: Robustness.**

## 6. reel-cli fixes (#33, #34, #35)

All in `reel-cli/src/main.rs`. #33 is a correctness issue (benign in practice). #34 and #35 are validation/usability.

### 33. reel-cli blocking stdin read on single-threaded async runtime

`reel-cli/src/main.rs` — `std::io::stdin().read_to_string()` blocks the `current_thread` tokio runtime. Works in practice since it runs before any async work, but should use `spawn_blocking` or async IO. **Category: Correctness.**

### 34. reel-cli `--timeout 0` accepted without validation

`reel-cli/src/main.rs` — No lower bound on timeout value. `Duration::from_secs(0)` causes instant timeout on every operation. **Category: Validation.**

### 35. reel-cli dry run output omits grant info and uses different JSON format

`reel-cli/src/main.rs` — Dry run uses `to_string_pretty` and omits the resolved `ToolGrant`; success output uses `to_string` (compact). Inconsistent format and missing diagnostic info. **Category: Usability.**

## 7. Ripgrep resolution tests (#3d, #3e, #3f)

Pure test gaps on a single code path (`resolve_rg_binary`).

### 3d. Tests assume `REEL_RG_PATH` is always set

Two tests use `^$env.REEL_RG_PATH` without guarding for absence. **Category: Testing.**

### 3e. No test for `rg_binary = None` branch

No test covers `resolve_rg_binary` returning `None`. **Category: Testing.**

### 3f. `resolve_rg_binary` has no direct unit tests

Tested only indirectly through integration tests. **Category: Testing.**

## 8. Custom tool dispatch (#48)

No practical impact — unsupported scenario, already guarded by `build_request_config`.

### 48. Duplicate custom tool names: HashMap changes first-match to last-match semantics

`reel/src/agent.rs` — `dispatch_tool` previously used linear scan (first match wins). The `HashMap<String, usize>` built via `collect()` keeps the last entry for duplicate keys. No practical impact: duplicate custom tool names are not a supported scenario, and `build_request_config` rejects duplicate names against built-ins. **Category: Testing.**

## 9. Grant model refinements (#51, #52)

### 51. `ToolGrant::READ` understates the flag's scope

`reel/src/tools.rs` — `READ` enables NuShell (arbitrary command execution) and the read-only built-in tools. The name suggests read-only access but actually enables command execution. A name like `TOOLS` would describe the scope better. **Category: Naming.**

### 52. `WRITE`/`NETWORK` → `READ` implication not enforced at type level

`reel/src/tools.rs` — The implication (WRITE implies READ, NETWORK implies READ) is enforced only in `from_names`. Library consumers constructing `ToolGrant` directly via bitflags can create bare `WRITE` without `READ`, which silently produces zero tool definitions. `tool_definitions` and `required_grant` defensively check `WRITE | READ` together, duplicating the invariant. Consider a normalizing constructor or custom `BitOr` to enforce at the type level. **Category: Separation of concerns.**

## 10. NuSession minor refinements (#55, #56, #57, #58, #59)

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

## 11. Cache directory naming (#61)

### 61. `cache_dir` field and `resolve_cache_dir` name understates the directory's role

`reel/src/nu_session.rs` — The `cache_dir` field and `resolve_cache_dir` function resolve the directory containing the nu binary, rg binary, and config files. This is a tool/asset directory, not a cache in the conventional sense. Names like `tool_dir` / `resolve_tool_dir` would better reflect the role. Inherited from the build-system name `nu-cache` / `NU_CACHE_DIR`. A rename would touch the field, function, all callers, tests, and docs. **Category: Naming.**

## 12. Network test helpers (#65, #66, #67)

### 65. `looks_like_sandbox_denial` keywords are broad

`reel/src/nu_session.rs` — The `looks_like_sandbox_denial` helper checks for generic keywords like `"denied"`, `"permission"`, `"blocked"` that could match non-sandbox errors (e.g. file permission errors). Narrowing to sandbox-specific phrases would reduce false positives but risks missing real denials. **Category: Testing.**

### 66. `looks_like_sandbox_denial` has no unit tests

`reel/src/nu_session.rs` — The helper is used by both network integration tests but has no dedicated unit tests with known sandbox denial messages and known non-denial messages. **Category: Testing.**

### 67. `http_responding_listener` name does not convey side effects

`reel/src/nu_session.rs` — The function spawns a background thread and returns a port number, but the name suggests it returns a listener. A name like `spawn_http_responder` would better convey the fire-and-forget nature. **Category: Naming.**
