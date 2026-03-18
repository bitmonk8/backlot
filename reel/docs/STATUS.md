# Project Status

## Current Phase

**Core agent runtime and tooling implemented. All 192 tests pass locally.** Lot dependency at rev `c3cc94d`. Flick dependency at rev `287bfbd` (adds Clone derives for config types). CI fully green: Windows, Linux, macOS. Linux CI runs tests in parallel (ETXTBSY fix in lot).

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds / 200 total tool calls, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on tool availability (built-in or custom). Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::READ`; write tools gated on `ToolGrant::WRITE | ToolGrant::READ`.
- **NuShell sandbox** (`nu_session.rs`) — `NuSession` managing a persistent `nu --mcp` process (JSON-RPC 2.0). Per-session temp directory under `<project_root>/.reel/tmp/`. Sandbox policy via `lot` (Windows AppContainer, Linux user/mount/pid namespaces, macOS Seatbelt). Grant-based process respawn if grants or project root change between calls. Non-blocking process teardown.
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
- **Test counts** — 192 tests total (181 reel + 11 reel-cli), all pass locally.
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

Remaining candidates: testing gaps (#3b, #3d, #3e, #3f, #3g, #3h, #6, #13, #39, #40, #41, #53, #56, #57, #58), naming (#54, #59), simplification (#55), correctness (#60), other (#51, #52).
