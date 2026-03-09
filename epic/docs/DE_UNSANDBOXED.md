# Remove Unsandboxed Execution Fallback — DONE

Unsandboxed fallback removed. If lot cannot set up a sandbox, the tool call fails with an error.

## Motivation

The unsandboxed fallback added code complexity (platform-specific process management, error classification, dual execution paths) for a degraded mode that undermines epic's security model. Lot provides diagnostic error messages when sandbox setup fails — a clear error telling the user what to fix is better than silently running without isolation.

## What Changed

- Removed from `src/agent/tools.rs` (~160 lines): `tool_bash_unsandboxed`, `SandboxSpawnError`, `classify_spawn_error`, `kill_process_tree`, `UNSANDBOXED_ENV_KEYS`, platform-specific `cfg` blocks, fallback branches in `tool_bash`, and 7 associated tests.
- Removed `libc` dependency from `Cargo.toml` (used only for `setsid()`/`kill()` in the unsandboxed path).
- `tool_bash` is now a straight pipeline: build policy → spawn sandboxed → return result or error.
- `tool_bash` returns `Result<BashOutput, String>` directly (no `SandboxSpawnError` enum).

## Platform Impact

- **Linux**: Requires unprivileged user namespaces. Users on Ubuntu 24.04+ (AppArmor restriction), corporate lockdowns (`kernel.unprivileged_userns_clone=0`), or restrictive Kubernetes pods get a clear error with lot's diagnostic message.
- **Windows**: AppContainer available on Windows 10+. Failure rare (restrictive Group Policy or antivirus interference).
- **macOS**: Seatbelt always available. Failure near-impossible.
- **GitHub Actions CI**: Ubuntu-latest runners support unprivileged user namespaces. No impact.
