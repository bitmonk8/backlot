# Windows Sandbox

Epic sandboxes the nu MCP process using [lot](https://github.com/bitmonk8/lot), which uses Windows AppContainer on Windows.

## What the Sandbox Needs

Agents need access to these directories only:

| Directory | Access | Purpose |
|-----------|--------|---------|
| Project root | Read or read-write (phase-dependent) | Project files |
| `%TEMP%` | Read-write | Scratch space |
| Cache dir | Read + execute | Nu binary, config files, rg binary |

All paths are user-owned. AppContainer ACL setup works without elevation. System directory grants (`include_platform_exec_paths`, `include_platform_lib_paths`) are not used — AppContainer inherits access to system DLLs (System32) without explicit grants.

## BUG: AppContainer Child Process Execution

External commands spawned by nu inside AppContainer (e.g., `^rg` via `epic grep`) fail with "Permission denied" despite `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE` ACLs on the cache directory containing the binary. This breaks `epic grep` (which wraps rg) and any other tool that shells out to an external binary.

**Impact**: `epic grep` is non-functional on Windows. This is the only tool affected in v1 (the other five tools are implemented as nu custom commands and do not spawn child processes).

**What we know**:
- The rg binary exists in the cache directory and has correct ACLs applied.
- `nu` itself launches fine from the same cache directory (it's the sandbox entrypoint, not a child process).
- The ACL grants `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE` via lot's `exec_path()`.
- Child process creation (not file read/execute) is what fails — the error is at process spawn time, not at binary load time.
- The test `integration_sandbox_read_only_prevents_writes` fails on Windows due to this bug. Reproduce with: `cargo test integration_sandbox_read_only_prevents_writes`.

**Investigation needed**:
- Does AppContainer require additional capabilities (e.g., `lpSecurityCapabilities`) for child process creation?
- Does lot need to add the AppContainer SID to the child binary's ACL differently than the parent?
- Does rg dynamically link a DLL from a directory not in the sandbox policy? The sandbox does not grant access to `%ProgramFiles%` or other system library paths. If rg depends on a DLL outside System32 (which AppContainer inherits), the load would fail at process spawn time with "Permission denied".
- Could rg be invoked differently (e.g., as a nu plugin instead of `^rg`)?

## For Epic Developers

### Sandbox Integration Tests

The `integration_sandbox_*` tests in `src/agent/nu_session.rs` call `session.evaluate()` directly and fail on sandbox setup failure (no silent skip). Tests pass without elevation.

Each test gets isolated project and cache directories via `sandbox_env()`:
- **Project dirs** use `target/sandbox-test/` (not `%TEMP%`) to avoid overlap with `include_temp_dirs()` write grants.
- **Cache dirs** are per-test copies of the build-time `target/nu-cache/` contents. This isolates AppContainer ACL operations so tests run in parallel without conflicts.

### Platform Notes

| Platform | Mechanism | Setup Required |
|----------|-----------|----------------|
| Windows | AppContainer + ACLs | None |
| Linux | User namespaces + seccomp | No — works if unprivileged user namespaces are enabled (kernel default) |
| macOS | Seatbelt (SBPL) | No — always available |
