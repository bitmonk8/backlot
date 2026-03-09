# Remove Unsandboxed Execution Fallback — DONE

Unsandboxed fallback removed. If lot cannot set up a sandbox, the tool call fails with an error.

## Motivation

The unsandboxed fallback added code complexity (platform-specific process management, error classification, dual execution paths) for a degraded mode that undermines epic's security model. Lot provides diagnostic error messages when sandbox setup fails — a clear error telling the user what to fix is better than silently running without isolation.

## What Changed

- Removed the unsandboxed execution fallback (~160 lines) and its `libc` dependency. If sandbox setup fails, the tool call now returns an error.
- Shell execution uses `tool_nu` (NuShell MCP session) — see [NUSHELL_MIGRATION.md](NUSHELL_MIGRATION.md).

## Platform Impact

- **Linux**: Requires unprivileged user namespaces. Users on Ubuntu 24.04+ (AppArmor restriction), corporate lockdowns (`kernel.unprivileged_userns_clone=0`), or restrictive Kubernetes pods get a clear error with lot's diagnostic message.
- **Windows**: AppContainer available on Windows 10+. Failure rare (restrictive Group Policy or antivirus interference).
- **macOS**: Seatbelt always available. Failure near-impossible.
- **GitHub Actions CI**: Ubuntu-latest runners support unprivileged user namespaces. No impact.
