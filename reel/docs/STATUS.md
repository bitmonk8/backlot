# Project Status

## Current Phase

**Core agent runtime and tooling implemented. All 179 tests pass locally.** Lot dependency at rev `c3cc94d`. Flick dependency at rev `287bfbd` (adds Clone derives for config types). CI fully green: Windows, Linux, macOS. Linux CI runs tests in parallel (ETXTBSY fix in lot).

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on `ToolGrant::NU`. Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::NU`; write tools gated on `ToolGrant::WRITE | ToolGrant::NU`.
- **NuShell sandbox** (`nu_session.rs`) — `NuSession` managing a persistent `nu --mcp` process (JSON-RPC 2.0). Per-session temp directory under `<project_root>/.reel/tmp/`. Sandbox policy via `lot` (Windows AppContainer, Linux user/mount/pid namespaces, macOS Seatbelt). Grant-based process respawn if grants or project root change between calls. Non-blocking process teardown.
- **Sandbox re-exports** (`sandbox.rs`) — `reel::sandbox` module re-exporting lot's prerequisite APIs (`grant_appcontainer_prerequisites`, `appcontainer_prerequisites_met`, `is_elevated`, etc.) and types (`SandboxPolicy`, `SandboxError`). Library consumers no longer need a direct lot dependency.
- **CLI binary** (`reel-cli`) — `reel run` (execute agent query with YAML config, stdin, dry-run) and `reel setup` (Windows AppContainer ACL prerequisites). Single-pass YAML config parsing: parse as `Value`, pop `grant` key, pass remainder to flick. Uses `reel::sandbox` for all platform prerequisite checks.
- **Build infrastructure** (`build.rs`) — Downloads prebuilt NuShell 0.111.0 and ripgrep 14.1.1 binaries for target platform, verifies SHA-256, caches in `target/nu-cache/`. Generates `reel_config.nu` and `reel_env.nu` for nu custom commands.
- **CI pipeline** — GitHub Actions: fmt, clippy, test, build on Ubuntu, macOS, Windows. Rust 1.93.1 toolchain. Dependencies use pinned git revs (lot, flick). Linux CI uses dynamic cgroup delegation (discovers runner's actual cgroup, enables controllers hierarchically, creates sibling cgroup).
- **Network control** (`nu_session.rs`, `tools.rs`) — `ToolGrant::NETWORK` flag gates sandbox network access. Network denied by default; requires explicit `network` grant in config. Closes issue #22.
- **Config API cleanup** — `build_request_config` uses clone-and-mutate (closes issue #27). CLI `parse_config` uses single-pass YAML parsing: parse as `Value`, pop `grant`, pass remainder to flick (closes issue #16).
- **Typed error types** — `GrantParseError` struct for `ToolGrant::from_names`. Re-exported from `reel::GrantParseError` (closes issue #30).
- **Test coverage expansion** — `ToolGrant::from_names` unit tests (issue #36), custom `ToolHandler` dispatch tests (issue #1), full tool execution path integration tests (issue #2), CLI `parse_config`/`emit_error` tests (issue #12), sandbox network denial integration tests (issue #37).
- **Test counts** — 179 tests total (168 reel + 11 reel-cli), all pass locally.

## What Is NOT Implemented

These are known gaps with no corresponding code:

- **ToolHandler consumer** — Trait exists but no real consumer yet. Design assumes epic's Research Service as first consumer.

## Design Choices (intentional constraints)

### NuShell as execution substrate

All 6 built-in tools execute through a shared NuShell session (custom commands or direct evaluation). Enables state persistence (cwd, variables, env) across tool calls within a session.

### Grant-based tool availability

Bitflags (`WRITE`, `NU`, `NETWORK`) determine tool list and sandbox policy. Binary decision — no per-tool grants. Network access denied by default; requires explicit `NETWORK` grant.

### Tool loop over streaming

Request-dispatch-response cycles up to 50 rounds. No streaming of partial model responses.

### Eager NuShell spawn

Process started at session creation (if NU granted), not on first use. Avoids startup cost during tool calls.

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

Remaining candidates: testing gaps (#3b, #3d, #3e, #3f, #3g, #3h, #6, #7, #13, #14, #15, #38, #39, #40, #41, #43, #44, #45, #46), simplification (#3c, #10, #11, #17, #18), other (#42, #47).
