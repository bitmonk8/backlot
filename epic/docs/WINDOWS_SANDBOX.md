# Windows Sandbox

Epic sandboxes the nu MCP process using [lot](https://github.com/bitmonk8/lot), which uses Windows AppContainer on Windows.

## What the Sandbox Needs

Agents need access to these directories only:

| Directory | Access | Purpose |
|-----------|--------|---------|
| Project root | Read or read-write (phase-dependent) | Project files |
| `%TEMP%` | Read-write | Scratch space |
| `target\nu-cache\` | Read + execute | Nu binary, config files, rg binary |

All three are user-owned. AppContainer ACL setup works without elevation.

Agents do not need access to system executables or installed programs. Epic runs verification (build, test, lint) itself — agents use the six provided tools (Read, Write, Edit, Glob, Grep, NuShell) and nothing else.

## Current Issue

`build_nu_sandbox_policy()` in `nu_session.rs` calls lot's `include_platform_exec_paths()` and `include_platform_lib_paths()`, which add:

| Directory | Lot method |
|-----------|------------|
| `%SYSTEMROOT%\System32` | `include_platform_exec_paths()` |
| `%ProgramFiles%` | `include_platform_lib_paths()` |
| `%ProgramFiles(x86)%` | `include_platform_lib_paths()` |

These are unnecessary for epic's use case and cause two problems:

1. **Expanded attack surface** — agents gain access to system executables they should not be able to run.
2. **Requires elevated setup** — AppContainer needs `WRITE_DAC` on each directory in the policy. These directories are owned by `TrustedInstaller`/`Administrators`, so `SetNamedSecurityInfoW` fails with `Access is denied (error 5)` unless the user first grants `WRITE_DAC` from an elevated shell.

## Fix

This is a fix to epic, not to lot. Lot's `WRITE_DAC` requirement is inherent to AppContainer — lot cannot avoid it. The problem is that epic opts into directories (System32, ProgramFiles) that require elevation to ACL. By not requesting those directories, epic sidesteps the elevation requirement entirely since all remaining paths (project root, temp, nu-cache) are user-owned.

**Change**: Remove the `include_platform_exec_paths()` and `include_platform_lib_paths()` calls from `build_nu_sandbox_policy()` in `src/agent/nu_session.rs`. This eliminates both the security concern and the elevated setup requirement.

If a nu built-in depends on a System32 binary internally, it will fail when the sandbox blocks access. The fix in that case is to grant access to that specific binary, not to all of System32.

## For Epic Developers

### Sandbox Integration Tests

The `integration_sandbox_*` tests in `src/agent/nu_session.rs` call `session.evaluate()` directly and fail on sandbox setup failure (no silent skip). After the fix, these tests should pass without elevation or workarounds.

### Platform Notes

| Platform | Mechanism | Setup Required |
|----------|-----------|----------------|
| Windows | AppContainer + ACLs | Workaround needed until fix (this document) |
| Linux | User namespaces + seccomp | No — works if unprivileged user namespaces are enabled (kernel default) |
| macOS | Seatbelt (SBPL) | No — always available |
