# Project Status

## Current Phase

**Core agent runtime and tooling implemented. All 169 tests pass locally.** Lot dependency updated to rev `a17cedf` which includes Linux namespace fixes (mount_proc, pivot_root, cwd ordering) and cgroup delegation improvements. CI pipeline updated with proper Linux cgroup delegation matching lot's working setup. Windows CI green. Linux and macOS CI pending validation after lot cwd fix.

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on `ToolGrant::NU`. Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::NU`; write tools gated on `ToolGrant::WRITE | ToolGrant::NU`.
- **NuShell sandbox** (`nu_session.rs`) — `NuSession` managing a persistent `nu --mcp` process (JSON-RPC 2.0). Per-session temp directory under `<project_root>/.reel/tmp/`. Sandbox policy via `lot` (Windows AppContainer, Linux user/mount/pid namespaces, macOS Seatbelt). Grant-based process respawn if grants or project root change between calls.
- **CLI binary** (`reel-cli`) — `reel run` (execute agent query with YAML config, stdin, dry-run) and `reel setup` (Windows AppContainer ACL prerequisites). Two-pass YAML config parsing: extract reel `grant` field, pass remainder to flick.
- **Build infrastructure** (`build.rs`) — Downloads prebuilt NuShell 0.111.0 and ripgrep 14.1.1 binaries for target platform, verifies SHA-256, caches in `target/nu-cache/`. Generates `reel_config.nu` and `reel_env.nu` for nu custom commands.
- **CI pipeline** — GitHub Actions: fmt, clippy, test, build on Ubuntu, macOS, Windows. Rust 1.93.1 toolchain. Dependencies use pinned git revs (lot, flick). Linux CI uses dynamic cgroup delegation (discovers runner's actual cgroup, enables controllers hierarchically, creates sibling cgroup).
- **Test counts** — 169 tests total, all pass locally.

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

### Lot Update to rev c1c4724

Updated lot from `4e478de` to `a17cedf` (45 commits). Key changes consumed by reel:
- **Linux seccomp fixes**: `chdir`/`fchdir` and `socketpair` syscalls were missing from the seccomp allowlist. Nu panicked on startup with `set_current_dir() PermissionDenied` (chdir blocked) and tokio's signal handler failed with `failed to create UnixStream` (socketpair blocked). The mount namespace constrains visible paths, so chdir is safe; socketpair is local IPC only.
- **Linux cwd ordering fix**: lot's `chdir(cwd)` was called before `finish_mount_namespace()`, but `pivot_root` inside that function does `chdir("/")` which overwrote the cwd. Fixed by moving `chdir(cwd)` to after `finish_mount_namespace()`.
- **Linux namespace fixes**: mount_proc moved to inner child (PID namespace member), stale procfs mounts cleared before fresh /proc mount, pivot_root ordering corrected.
- **Cgroup delegation**: Dynamic cgroup discovery via `/proc/self/cgroup`, hierarchical controller enablement, sibling cgroup model respecting cgroupv2 no-internal-processes constraint.
- **CI alignment**: Reel CI updated to use lot's proven cgroup setup (dynamic discovery instead of hardcoded root-level delegation).

## CI Status

| Job | Status | Notes |
|-----|--------|-------|
| Format | pass | |
| Clippy (all 3) | pass | |
| Build (all 3) | pass | |
| Test (Windows) | pass | 169 pass |
| Test (Linux) | investigating | Root cause identified: lot's kill() does not terminate the inner child (see below) |
| Test (macOS) | fail | 4 failures — nu_glob intermediate directory traversal (issue #9c) |

### Linux CI progress

Three lot bugs fixed to get nu running inside Linux sandbox:
1. **chdir after pivot_root** (lot rev `87af454`): `chdir(cwd)` was before `finish_mount_namespace()` which does `pivot_root + chdir("/")`, overwriting the cwd.
2. **chdir in seccomp** (lot rev `c1c4724`): `SYS_chdir`/`SYS_fchdir` missing from allowlist. Nu's startup `set_current_dir()` returned EPERM.
3. **socketpair in seccomp** (lot rev `a17cedf`): `SYS_socketpair` missing from allowlist. Tokio's signal handler creates UnixStream via `socketpair()`.

After all three fixes, `nu -c 'echo hello'` works correctly inside the sandbox. `nu --mcp` starts and responds to MCP initialize.

#### Root cause: lot's kill() does not terminate the inner child

The full test suite hangs because lot's `kill()` sends SIGKILL to the **helper process**, but the helper is NOT PID 1 in the PID namespace. Lot uses `unshare(CLONE_NEWPID)` (line 169 in `lot/src/linux/mod.rs`), which does NOT move the calling process into the new namespace — only future children are placed in it. So:

- **Helper**: stays in the parent PID namespace (NOT PID namespace init)
- **Inner child** (nu): is PID 1 in the new PID namespace (confirmed by comment on line 230)
- `kill(helper_pid, SIGKILL)` kills the helper, but the inner child is orphaned (reparented to system init) and **keeps running**
- Nu holds the write ends of stdout/stderr pipes, so `read_line()` blocks forever

The comment on lot line 410-411 ("The helper is PID namespace init; killing it collapses the namespace") is **incorrect**.

**Evidence from CI**: The `diag_open_with_stderr` test was the last test to start (alphabetically after `diag_open_vs_alternatives`). It completed its `rpc_call` operations, then hung at `stderr_handle.join()` because the killed helper's inner child (nu) kept the stderr pipe open. The run was cancelled after 6+ hours.

**Impact on reel**: Any test or production use that calls `kill()` or triggers a timeout will leave nu processes orphaned. The `integration_timeout_kills_process` test will also hang because:
1. Timeout fires → `kill()` called → helper dies, nu survives
2. `spawn_blocking` task blocked on `read_line` from orphaned nu → leaked
3. Test function returns → tokio runtime shutdown waits for leaked task → hang

**Fix required in lot**: Either:
- (A) Track the inner child PID and kill it directly (lot knows `inner_pid` from the fork return)
- (B) Use `clone(CLONE_NEWPID)` instead of `fork()+unshare()` so the helper IS PID 1
- (C) Have the helper close child pipe FDs after fork AND kill inner child before exiting

Diagnostic tests `diag_kill_closes_pipes` and updated `diag_open_with_stderr` are deployed to verify this hypothesis in CI.

### macOS CI failures (4 tests)

Same nu_glob ancestor traversal issue as issue #9c. `reel read`, `reel write`, `reel edit` custom commands use `open`/`ls` which go through nu_glob's component-by-component path walk. macOS Seatbelt sandbox doesn't grant intermediate directory access. Plus `rg.exe` not found (wrong binary name for macOS in one test).

## Work Candidates

### Re-export lot's sandbox prerequisite APIs (issue #19)

Lot rev `a17cedf` grants traverse ACEs on user-owned ancestor directories automatically at spawn time. System directories (e.g., `C:\Users`) still require a one-time elevated setup via `grant_appcontainer_prerequisites`. Reel does not re-export these APIs, so library consumers cannot implement their own elevated setup command without a direct lot dependency. Add a `reel::sandbox` module re-exporting the prerequisite APIs, `is_elevated()`, and the `PrerequisitesNotMet` error variant. Update `reel-cli` to use the re-exports.
