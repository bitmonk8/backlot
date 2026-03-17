# Project Status

## Current Phase

**Core agent runtime and tooling implemented. All 170 tests pass locally.** Lot dependency updated to rev `1a6fc30` which includes Linux namespace fixes (mount_proc, pivot_root, cwd ordering), cgroup delegation improvements, seccomp allowlist fixes, and PR_SET_PDEATHSIG for reliable inner child termination. CI pipeline updated with proper Linux cgroup delegation matching lot's working setup. Windows CI green (170 pass). Linux CI green (170 pass). macOS CI has 3 failures (nu_glob intermediate directory traversal, issue #9c).

## What Is Implemented

- **Agent runtime** (`agent.rs`) — `Agent` struct managing single sessions with configurable grants and timeout. Tool loop runs up to 50 rounds, dispatching to built-in or custom handlers via `ToolHandler` trait. Structured vs. tool-loop routing based on `ToolGrant::NU`. Per-session timeout with model resume cancellation on expiry.
- **Built-in tools** (6 total, `tools.rs`) — `Read`, `Write`, `Edit`, `Glob`, `Grep` (all execute as nu custom commands: `reel read`, `reel write`, etc.), `NuShell` (direct evaluation). Read-only tools gated on `ToolGrant::NU`; write tools gated on `ToolGrant::WRITE | ToolGrant::NU`.
- **NuShell sandbox** (`nu_session.rs`) — `NuSession` managing a persistent `nu --mcp` process (JSON-RPC 2.0). Per-session temp directory under `<project_root>/.reel/tmp/`. Sandbox policy via `lot` (Windows AppContainer, Linux user/mount/pid namespaces, macOS Seatbelt). Grant-based process respawn if grants or project root change between calls.
- **CLI binary** (`reel-cli`) — `reel run` (execute agent query with YAML config, stdin, dry-run) and `reel setup` (Windows AppContainer ACL prerequisites). Two-pass YAML config parsing: extract reel `grant` field, pass remainder to flick.
- **Build infrastructure** (`build.rs`) — Downloads prebuilt NuShell 0.111.0 and ripgrep 14.1.1 binaries for target platform, verifies SHA-256, caches in `target/nu-cache/`. Generates `reel_config.nu` and `reel_env.nu` for nu custom commands.
- **CI pipeline** — GitHub Actions: fmt, clippy, test, build on Ubuntu, macOS, Windows. Rust 1.93.1 toolchain. Dependencies use pinned git revs (lot, flick). Linux CI uses dynamic cgroup delegation (discovers runner's actual cgroup, enables controllers hierarchically, creates sibling cgroup).
- **Test counts** — 170 tests total, all pass locally.

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
| Test (Windows) | pass | 170 pass |
| Test (Linux) | pass | 170 pass |
| Test (macOS) | fail | 3 failures — nu_glob intermediate directory traversal (issue #9c) |

### Linux CI progress

Three lot bugs fixed to get nu running inside Linux sandbox:
1. **chdir after pivot_root** (lot rev `87af454`): `chdir(cwd)` was before `finish_mount_namespace()` which does `pivot_root + chdir("/")`, overwriting the cwd.
2. **chdir in seccomp** (lot rev `c1c4724`): `SYS_chdir`/`SYS_fchdir` missing from allowlist. Nu's startup `set_current_dir()` returned EPERM.
3. **socketpair in seccomp** (lot rev `a17cedf`): `SYS_socketpair` missing from allowlist. Tokio's signal handler creates UnixStream via `socketpair()`.

After all three fixes, `nu -c 'echo hello'` works correctly inside the sandbox. `nu --mcp` starts and responds to MCP initialize.

#### Fixed: lot's kill() now terminates the inner child (lot rev `1a6fc30`)

Root cause was that lot used `unshare(CLONE_NEWPID)` which does NOT place the helper in the new PID namespace. The inner child (nu) is PID 1 in the namespace. Killing the helper orphaned the inner child instead of collapsing the namespace. When `NuProcess` was trapped in a `spawn_blocking` closure (timeout path), stdin remained open, nu stayed alive, and `read_line()` blocked indefinitely.

Two-layer fix applied:
- **lot (rev `1a6fc30`)**: `prctl(PR_SET_PDEATHSIG, SIGKILL)` in the inner child before exec. The kernel automatically SIGKILLs the inner child when the helper dies.
- **reel (defense-in-depth)**: `NuProcess::stdin` changed to `Arc<Mutex<Option<File>>>` (`StdinHandle`). Stored in `SessionState::inflight_stdin` during `evaluate_inner` Phase 2. `kill()` takes and drops the File to close the pipe, triggering EOF on nu even if the lot-level kill fails.

CI results after fix: 168 pass, 2 fail (unrelated), finished in 7s (was 70s before fix, infinite before diagnostics).

### Linux CI remaining failures (2 tests) — FIXED

Both failures were platform-specific test issues, not sandbox bugs:

- `integration_sandbox_rg_with_ancestor_traverse` — hardcoded `rg.exe` binary name. On Linux the binary is `rg`, not `rg.exe`. The isolated cache copy contained `rg` but the test looked for `rg.exe`. Fixed: use `cfg!(windows)` conditional for binary name.
- `integration_sandbox_temp_dir_no_pivot_to_project` — asserted `is_error` on nu's `cp` output, but nu's `cp` on Linux doesn't report an error when writing to a read-only bind mount (returns empty list instead). The sandbox correctly prevents the write (file does not exist). Fixed: check filesystem state (file non-existence) as primary assertion instead of relying on nu error reporting.

### macOS CI failures (4 tests)

Same nu_glob ancestor traversal issue as issue #9c. `reel read`, `reel write`, `reel edit` custom commands use `open`/`ls` which go through nu_glob's component-by-component path walk. macOS Seatbelt sandbox doesn't grant intermediate directory access. The `rg.exe` binary name issue (wrong name for non-Windows) is now fixed.

## Work Candidates

### Re-export lot's sandbox prerequisite APIs (issue #19)

Lot rev `1a6fc30` grants traverse ACEs on user-owned ancestor directories automatically at spawn time. System directories (e.g., `C:\Users`) still require a one-time elevated setup via `grant_appcontainer_prerequisites`. Reel does not re-export these APIs, so library consumers cannot implement their own elevated setup command without a direct lot dependency. Add a `reel::sandbox` module re-exporting the prerequisite APIs, `is_elevated()`, and the `PrerequisitesNotMet` error variant. Update `reel-cli` to use the re-exports.
