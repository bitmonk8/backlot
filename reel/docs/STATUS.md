# Project Status

## Current Phase

**Core agent runtime and tooling implemented. Diagnosing 3 failing nu custom command tests (ISSUES.md #9c).**

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on `ToolGrant::NU`. Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::NU`; write tools gated on `ToolGrant::WRITE | ToolGrant::NU`.
- **NuShell sandbox** (`nu_session.rs`) — `NuSession` managing a persistent `nu --mcp` process (JSON-RPC 2.0). Per-session temp directory under `<project_root>/.reel/tmp/`. Sandbox policy via `lot` (Windows AppContainer). Grant-based process respawn if grants or project root change between calls.
- **CLI binary** (`reel-cli`) — `reel run` (execute agent query with YAML config, stdin, dry-run) and `reel setup` (Windows AppContainer ACL prerequisites). Two-pass YAML config parsing: extract reel `grant` field, pass remainder to flick.
- **Build infrastructure** (`build.rs`) — Downloads prebuilt NuShell 0.111.0 and ripgrep 14.1.1 binaries for target platform, verifies SHA-256, caches in `target/nu-cache/`. Generates `reel_config.nu` and `reel_env.nu` for nu custom commands.
- **CI pipeline** — GitHub Actions: fmt, clippy, test, build on Ubuntu, macOS, Windows. Rust 1.93.1 toolchain. Dependencies use pinned git revs (lot, flick).
- **Test counts** — 145 tests total: 142 pass, 3 fail (AppContainer sandbox access issues in `reel read`/`write`/`edit` custom command tests — see ISSUES.md #9c).

## What Is NOT Implemented

These are known gaps with no corresponding code:

- **Network control** — Sandbox always allows network. Should be gated by grant or policy (issue #22).
- **Proper error types** — `ToolGrant::from_names` returns `Result<_, String>`. Should use typed errors (issue #30).
- **lot re-export** — `reel-cli` depends on `lot` directly for AppContainer checks. Should be re-exported via `reel::sandbox` (issue #19).
- **Config API mutations** — Flick's `RequestConfig` cannot be mutated post-parse; reel reconstructs via serialization workaround (issue #27).
- **ToolHandler consumer** — Trait exists but no real consumer yet. Design assumes epic's Research Service as first consumer.

## Design Choices (intentional constraints)

### NuShell as execution substrate

All 6 built-in tools execute through a shared NuShell session (custom commands or direct evaluation). Enables state persistence (cwd, variables, env) across tool calls within a session.

### Grant-based tool availability

Bitflags (`WRITE`, `NU`) determine tool list and sandbox policy. Binary decision — no per-tool grants.

### Tool loop over streaming

Request-dispatch-response cycles up to 50 rounds. No streaming of partial model responses.

### Eager NuShell spawn

Process started at session creation (if NU granted), not on first use. Avoids startup cost during tool calls.

### Dual-crate architecture

Library (`reel`) + thin CLI (`reel-cli`). Follows flick's pattern for testability and reusability.

## Completed Work

### Initial Implementation

Core agent runtime, 6 built-in tools, NuShell sandbox session, CLI binary, and build infrastructure for cross-platform binary downloads. Extracted from epic as standalone workspace.

### CI Pipeline

GitHub Actions on three platforms. Pinned git rev dependencies (lot, flick) replacing local path dependencies. `.gitattributes` with `eol=lf` for cross-platform `rustfmt` consistency.

### Nu 0.111.0 Compatibility

Fixed `reel_config.nu` for nu 0.111.0 — removed obsolete `--string` flag from `str replace` calls.

### Lot Policy Fix

Updated lot dependency to rev with directional policy overlap support, fixing 5 sandbox policy tests that failed when write-path children existed under read-path parents.

## Work Candidates

### Diagnose issue #9c: reel read/write/edit fail inside AppContainer

`reel read`, `reel write`, `reel edit` nu custom commands fail when executed inside the AppContainer sandbox. `reel glob` and `reel grep` pass in the same environment. The failures are in nu's built-in file commands (`ls`, `open`, `save`) resolving paths inside AppContainer. Root cause not yet identified. See ISSUES.md #9c for details and eliminated hypotheses.
