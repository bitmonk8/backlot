# Project Status

## Current Phase

**Core agent runtime and tooling implemented. All 197 tests pass locally.** Lot dependency at rev `c3cc94d`. Flick dependency at rev `287bfbd` (adds Clone derives for config types). CI fully green: Windows, Linux, macOS. Linux CI runs tests in parallel (ETXTBSY fix in lot).

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds / 200 total tool calls, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on tool availability (built-in or custom). Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::READ`; write tools gated on `ToolGrant::WRITE | ToolGrant::READ`.
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
- **Grant model cleanup** — Renamed `ToolGrant::NU` → `ToolGrant::READ`. `WRITE` and `NETWORK` now imply `READ` in `from_names`. Config accepts `"read"` instead of `"nu"`. Closes issue #25.
- **Agent dispatch and tool-loop semantics** — `run()` dispatch uses tool availability (built-in + custom) instead of `ToolGrant::READ` (issue #5). Per-session tool call cap `MAX_TOOL_CALLS = 200` (issue #24). Tests for `ToolCallsPending` in structured mode (issue #14), multi-tool-call-per-round counting (issue #15), custom-tools-only routing, and tool call cap exceeded.
- **NuSession process lifecycle hardening** — Fixed process steal race in `evaluate_inner` by combining ensure+take into atomic `ensure_and_take` (issue #47). Removed `eprintln!` from library `NuProcess::drop` (issue #42). Extracted `bounded_reap` as testable function (issue #43). Added respawn tests for project root change (#7), NETWORK grant change (#38), `spawn()` parameter mismatch (#45). Added concurrent evaluate test (#44), kill-during-evaluate test (#46). Added Windows stabilization delay for flaky timeout test (#50).
- **Runtime cache directory resolution** — Replaced compile-time `option_env!("NU_CACHE_DIR")` in `NuSession::new()` with runtime `resolve_cache_dir()` that first checks next to the current executable, then falls back to the compile-time path. Fixes binary relocation breaking config file resolution (issue #32).
- **NuSession temp dir cleanup** — `SessionTempDir` wrapper cleans up empty `.reel/tmp/` and `.reel/` parent directories on drop, eliminating visible side effects in user project directories (issue #29). Added tests for parent cleanup and sibling preservation (issue #3b). Removed unused `cache` parameter from `policy_test_fixture` helper (issue #49).
- **Test isolation hardening** — `isolated_session()` and `tmp_sandbox_cache()` now panic instead of silently falling back to unsandboxed `NuSession::new()` when `NU_CACHE_DIR` is not set at compile time (issue #3g). Doc comments on `NuSession::new()`, `NuSession::with_cache_dir()`, `isolated_session()`, and `SandboxTestEnv` warn against direct construction in tests (issue #3h). Network integration tests replaced external `httpbin.org` dependency with local loopback `TcpListener` for deterministic sandbox denial/allowance verification (issue #39).
- **Test counts** — 199 tests total (188 reel + 11 reel-cli), all pass locally.
- **Documentation** — End-user `README.md` and developer `docs/DESIGN.md` written following sibling project conventions (lot, flick, epic). Obsolete spec docs (`docs/CLI_TOOL.md`, `docs/CLI_TOOL_INTEGRATION_TESTS.md`) deleted — all content integrated into README and DESIGN.

## What Is NOT Implemented

These are known gaps with no corresponding code:

- **ToolHandler consumer** — Trait exists but no real consumer yet. Design assumes epic's Research Service as first consumer.

## Design Choices (intentional constraints)

### NuShell as execution substrate

All 6 built-in tools execute through a shared NuShell session (custom commands or direct evaluation). Enables state persistence (cwd, variables, env) across tool calls within a session.

### Grant-based tool availability

Bitflags (`READ`, `WRITE`, `NETWORK`) determine tool list and sandbox policy. Binary decision — no per-tool grants. `WRITE` and `NETWORK` imply `READ`. Network access denied by default; requires explicit `NETWORK` grant.

### Tool loop over streaming

Request-dispatch-response cycles up to 50 rounds. No streaming of partial model responses.

### Eager NuShell spawn

Process started at session creation (if READ granted), not on first use. Avoids startup cost during tool calls.

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

Ordered by planned execution. Clusters group tightly-related issues for single-PR batches.

### Batch 1: Network test robustness (#62, #63, #64)
Tests currently provide false confidence. #62: allowed-network test never exercises its assertion. #63: fragile string-matching heuristic for sandbox denial. #64: denied test accepts any error as proof of blocking.

### Batch 2: Naming (#54, #59, #61, #51)
Mechanical renames, no behavioral change. #54: misleading agent test name. #59: misleading bounded_reap test name. #61: `cache_dir`/`resolve_cache_dir` understates directory role. #51: `ToolGrant::READ` understates scope.

### Batch 3: reel-cli fixes (#33, #34, #35)
All in `reel-cli/src/main.rs`. #33: blocking stdin read on async runtime. #34: `--timeout 0` accepted without validation. #35: dry-run output inconsistency.

### Batch 4: Tool execution coverage (#40, #41)
Test additions only. #40: missing Edit/Grep end-to-end tests via `execute_tool()`. #41: `from_names` empty-string element untested.

### Batch 5: Ripgrep resolution tests (#3d, #3e, #3f)
Three test gaps on `resolve_rg_binary`. Very contained.

### Batch 6: Agent test gaps (#53, #6, #13)
#53: boundary test for exactly 200 tool calls. #6: timeout during resume phase. #13: `RunResult` field propagation.

### Remaining (unscheduled)
NuSession stderr capture (#23), public API surface (#31, #8), NuSession internals (#55, #56, #57, #58, #60), grant model (#52).
