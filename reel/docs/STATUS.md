# Project Status

## Current Phase

**Core agent runtime and tooling implemented. All 258 tests pass locally.** Lot dependency at rev `c3cc94d`. Flick dependency at rev `287bfbd` (adds Clone derives for config types). CI fully green: Windows, Linux, macOS. Linux CI runs tests in parallel (ETXTBSY fix in lot).

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds / 200 total tool calls, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on tool availability (built-in or custom). Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::TOOLS`; write tools gated on `ToolGrant::WRITE | ToolGrant::TOOLS`.
- **NuShell sandbox** (`nu_session.rs`) — `NuSession` managing a persistent `nu --mcp` process (JSON-RPC 2.0). Per-session temp directory under `<project_root>/.reel/tmp/` with automatic parent cleanup on drop. Sandbox policy via `lot` (Windows AppContainer, Linux user/mount/pid namespaces, macOS Seatbelt). Grant-based process respawn if grants or project root change between calls. Non-blocking process teardown.
- **Sandbox re-exports** (`sandbox.rs`) — `reel::sandbox` module re-exporting lot's prerequisite APIs (`grant_appcontainer_prerequisites`, `appcontainer_prerequisites_met`, `is_elevated`, etc.) and types (`SandboxPolicy`, `SandboxError`). Library consumers no longer need a direct lot dependency.
- **CLI binary** (`reel-cli`) — `reel run` (execute agent query with YAML config, stdin, dry-run) and `reel setup` (Windows AppContainer ACL prerequisites). Single-pass YAML config parsing: parse as `Value`, pop `grant` key, pass remainder to flick. Uses `reel::sandbox` for all platform prerequisite checks.
- **Build infrastructure** (`build.rs`) — Downloads prebuilt NuShell 0.111.0 and ripgrep 14.1.1 binaries for target platform, verifies SHA-256, caches in `target/nu-cache/`. Generates `reel_config.nu` and `reel_env.nu` for nu custom commands.
- **CI pipeline** — GitHub Actions: fmt, clippy, test, build on Ubuntu, macOS, Windows. Rust 1.93.1 toolchain. Dependencies use pinned git revs (lot, flick). Linux CI uses dynamic cgroup delegation (discovers runner's actual cgroup, enables controllers hierarchically, creates sibling cgroup).
- **Network control** (`nu_session.rs`, `tools.rs`) — `ToolGrant::NETWORK` flag gates sandbox network access. Network denied by default; requires explicit `network` grant in config. Closes issue #22.
- **Config API cleanup** — `build_request_config` uses clone-and-mutate (closes issue #27). CLI `parse_config` uses single-pass YAML parsing: parse as `Value`, pop `grant`, pass remainder to flick (closes issue #16).
- **Typed error types** — `GrantParseError` struct for `ToolGrant::from_names`. Re-exported from `reel::GrantParseError` (closes issue #30).
- **Test coverage expansion** — `ToolGrant::from_names` unit tests (issue #36), custom `ToolHandler` dispatch tests (issue #1), full tool execution path integration tests (issue #2), CLI `parse_config`/`emit_error` tests (issue #12), sandbox network denial integration tests (issue #37).
- **Simplification batch** — Policy test helper `policy_test_fixture` deduplicates sandbox policy test setup (issue #3c). `extract_text` uses reverse iterator (issue #10). `dispatch_tool` uses `HashMap<String, usize>` index for O(1) custom tool lookup (issue #11). CLI prerequisite path resolution extracted to `resolve_prerequisite_paths` (issue #17). `build_request_config` is the single public config-building method on `Agent` (issue #18).
- **Grant model cleanup** — Renamed `ToolGrant::NU` → `ToolGrant::TOOLS`. `WRITE` and `NETWORK` now imply `TOOLS` in `from_names`. Config accepts `"tools"` instead of `"nu"`. Closes issue #25.
- **Agent dispatch and tool-loop semantics** — `run()` dispatch uses tool availability (built-in + custom) instead of grant flags (issue #5). Per-session tool call cap `MAX_TOOL_CALLS = 200` (issue #24). Tests for `ToolCallsPending` in structured mode (issue #14), multi-tool-call-per-round counting (issue #15), custom-tools-only routing, and tool call cap exceeded.
- **NuSession process lifecycle hardening** — Fixed process steal race in `evaluate_inner` by combining ensure+take into atomic `ensure_and_take` (issue #47). Removed `eprintln!` from library `NuProcess::drop` (issue #42). Extracted `bounded_reap` as testable function (issue #43). Added respawn tests for project root change (#7), NETWORK grant change (#38), `spawn()` parameter mismatch (#45). Added concurrent evaluate test (#44), kill-during-evaluate test (#46). Added Windows stabilization delay for flaky timeout test (#50).
- **Runtime tool directory resolution** — Replaced compile-time `option_env!("NU_CACHE_DIR")` in `NuSession::new()` with runtime `resolve_tool_dir()` that first checks next to the current executable, then falls back to the compile-time path. Fixes binary relocation breaking config file resolution (issue #32).
- **NuSession temp dir cleanup** — `SessionTempDir` wrapper cleans up empty `.reel/tmp/` and `.reel/` parent directories on drop, eliminating visible side effects in user project directories (issue #29). Added tests for parent cleanup and sibling preservation (issue #3b). Removed unused `cache` parameter from `policy_test_fixture` helper (issue #49).
- **Test isolation hardening** — `isolated_session()` and `tmp_sandbox_tool_dir()` now panic instead of silently falling back to unsandboxed `NuSession::new()` when `NU_CACHE_DIR` is not set at compile time (issue #3g). Doc comments on `NuSession::new()`, `NuSession::with_tool_dir()`, `isolated_session()`, and `SandboxTestEnv` warn against direct construction in tests (issue #3h). Network integration tests replaced external `httpbin.org` dependency with local loopback `TcpListener` for deterministic sandbox denial/allowance verification (issue #39).
- **Network test robustness** — `looks_like_sandbox_denial` shared helper with expanded keyword list covering macOS Seatbelt, Windows AppContainer, and Linux seccomp/namespace denial wording (issue #63). Allowed-network test uses `spawn_http_responder` that sends a real HTTP 200 response, ensuring the `Ok` path is exercised and the sandbox-denial assertion is tested (issue #62). Denied-network test verifies sandbox-specific error wording on platforms with active enforcement, logs a warning otherwise (issue #64).
- **Naming cleanup batch** — Renamed `ToolGrant::READ` → `ToolGrant::TOOLS` (issue #51). Renamed `cache_dir`/`resolve_cache_dir`/`with_cache_dir` → `tool_dir`/`resolve_tool_dir`/`with_tool_dir` (issue #61). Renamed misleading test `run_with_tools_counts_multi_tool_rounds` → `run_with_tools_counts_multi_calls_in_round` (issue #54). Renamed misleading test `bounded_reap_returns_true_on_immediate_exit` → `bounded_reap_returns_true_on_error` (issue #59).
- **CLI fixes batch** — Blocking stdin read wrapped in `spawn_blocking` (issue #33). `--timeout 0` rejected with validation error (issue #34). Dry-run output uses compact JSON and includes resolved grant names (issue #35).
- **Tool execution coverage batch** — Edit and Grep end-to-end integration tests via `execute_tool()` (issue #40). `ToolGrant::from_names` empty-string rejection test (issue #41).
- **Ripgrep resolution tests** — `resolve_rg_binary` direct unit test for tool_dir lookup (issue #3f), None-when-absent branch (issue #3e), compile-time tool dir validation (issue #3d).
- **Agent test gap coverage** — Boundary test for exactly `MAX_TOOL_CALLS` (200) succeeding (issue #53). Timeout during `client.resume()` in tool loop (issue #6). `RunResult` usage/response_hash propagation in both structured and tool-loop modes (issue #13).
- **NuSession minor refinements** — Extracted `SessionState::register_inflight` helper to deduplicate 3x inflight registration in `ensure_and_take` (issue #55). Added `bounded_reap_returns_true_on_normal_exit` test for `Ok(Some(ExitStatus))` path (issue #58). Added comments documenting untestable retry branch (issue #56), concurrent evaluate limitation (issue #57), and singleton inflight handle limitation (issue #60).
- **Network test helper improvements** — Narrowed `looks_like_sandbox_denial` keywords from generic single words to sandbox-specific multi-word phrases to reduce false positives (issue #65). Added unit tests for `looks_like_sandbox_denial` covering known denial messages and known non-denial messages (issue #66). Renamed `http_responding_listener` to `spawn_http_responder` (issue #67).
- **ToolGrant normalization** — `ToolGrant::normalize()` enforces WRITE-implies-TOOLS and NETWORK-implies-TOOLS at the type level. Called at `tool_definitions()` entry point so bare `ToolGrant::WRITE` produces correct tool definitions. `to_names()` coverage guard test catches future flag additions (issues #52, #69).
- **Agent test consolidation** — `SlowProvider` and `FastThenSlowProvider` consolidated into `DelayProvider` with configurable `fast_calls` count (issue #71). `text_response()` and `tool_call_response()` helpers eliminate repeated `ModelResponse` boilerplate (issue #72). Boundary test for exactly 201 (MAX_TOOL_CALLS + 1) tool calls verifies `>` vs `>=` semantics (issue #73). Duplicate custom tool name HashMap semantics documented with test (issue #48).
- **Test infrastructure cleanup** — `resolve_rg_binary_with_compile_time_tool_dir` converted from silent skip to unconditional assertions (issue #70). Regression assertions for removed sandbox denial keywords added (issue #74). CLI dry-run test exercises `build_dry_run_output` helper extracted from `cmd_run` (issue #68).
- **Test coverage gaps batch** — Mid-round tool call cap boundary test verifies `>` check when cumulative count crosses 200 within a round (issue #75). CLI dry-run test asserts specific tool names (Read, Write, Edit, Glob, Grep, NuShell) not just non-empty (issue #76). Duplicate custom tool name test renamed and documented as defense-in-depth using production index construction path (issue #77).
- **Public API cleanup** — Removed `pub use nu_session::NuSession` re-export from lib.rs (no external consumer, still accessible via `reel::nu_session::NuSession`) (issue #8). Marked `test_support` module `#[doc(hidden)]` with unstable-API warning (issue #31).
- **Tool robustness** — All file tools (Read, Write, Edit, Glob, Grep) now accept an optional `timeout` parameter (default 120s, max 600s), matching the existing NuShell tool behavior (issue #26). `reel glob` has a default depth limit of 20 preventing runaway traversal in deep trees with symlink cycles; model can override via `depth` parameter (issue #28).
- **Timeout schema deduplication** — Extracted `with_timeout()` helper in `tools.rs`, replacing 6 identical inline timeout JSON property fragments (issue #78). Added `parse_timeout` unit tests covering default, valid, clamped, zero, non-integer, and boundary values (issue #79).
- **Timeout correctness and coverage** — `parse_timeout` now clamps to `[1, MAX_NU_TIMEOUT_SECS]`, preventing zero-timeout immediate expiration (issue #80). Added tests for negative number fallback, `with_timeout` description field, non-object properties value, exact-minimum boundary, and float fallback (issues #81, #82, #83).
- **NuSession stderr capture** — Nu stderr is now piped (not discarded) and captured by a background reader thread into a shared `Arc<Mutex<String>>` buffer. `drain_stderr` helper drains accumulated content. `NuOutput` gains `stderr: Option<String>` field populated on both success and error paths. RPC error messages include stderr content when available. Buffer capped at 64 KiB with oldest-line eviction via `append_capped`. Closes issues #23, #85, #86, #87, #88.
- **Test counts** — 258 tests total (243 reel + 15 reel-cli), all pass locally.
- **Documentation** — End-user `README.md` and developer `docs/DESIGN.md` written following sibling project conventions (lot, flick, epic). Obsolete spec docs (`docs/CLI_TOOL.md`, `docs/CLI_TOOL_INTEGRATION_TESTS.md`) deleted — all content integrated into README and DESIGN.

## What Is NOT Implemented

These are known gaps with no corresponding code:

- **ToolHandler consumer** — Trait exists but no real consumer yet. Design assumes epic's Research Service as first consumer.

## Design Choices (intentional constraints)

### NuShell as execution substrate

All 6 built-in tools execute through a shared NuShell session (custom commands or direct evaluation). Enables state persistence (cwd, variables, env) across tool calls within a session.

### Grant-based tool availability

Bitflags (`TOOLS`, `WRITE`, `NETWORK`) determine tool list and sandbox policy. Binary decision — no per-tool grants. `WRITE` and `NETWORK` imply `TOOLS`. Network access denied by default; requires explicit `NETWORK` grant.

### Tool loop over streaming

Request-dispatch-response cycles up to 50 rounds. No streaming of partial model responses.

### Eager NuShell spawn

Process started at session creation (if TOOLS granted), not on first use. Avoids startup cost during tool calls.

### Dual-crate architecture

Library (`reel`) + thin CLI (`reel-cli`). Follows flick's pattern for testability and reusability.

## CI Status

| Job | Status | Notes |
|-----|--------|-------|
| Format | pass | |
| Clippy (all 3) | pass | |
| Build (all 3) | pass | |
| Test (Windows) | pass | |
| Test (Linux) | pass | |
| Test (macOS) | pass | |

## Work Candidates

No remaining work candidates.
