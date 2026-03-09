# Remove Unsandboxed Execution Fallback

Remove the unsandboxed fallback from epic's shell tool. If lot cannot set up a sandbox, the tool call fails with an error.

## Motivation

The unsandboxed fallback adds code complexity (platform-specific process management, error classification, dual execution paths) for a degraded mode that undermines epic's security model. Lot provides diagnostic error messages when sandbox setup fails — a clear error telling the user what to fix is better than silently running without isolation.

## Code Removed

### `src/agent/tools.rs`

| Component | Lines | Count |
|---|---|---|
| `tool_bash_unsandboxed` function | 709–765 | 57 |
| `UNSANDBOXED_ENV_KEYS` constant | 701–706 | 6 |
| `kill_process_tree` (unix + windows) | 769–787 | 18 |
| `SandboxSpawnError` enum | 634–640 | 7 |
| `classify_spawn_error` function | 680–697 | 18 |
| Fallback branches in `tool_bash` | 617–620, 626–629 | 8 |
| Platform-specific `cfg` blocks (`setsid`, `CREATE_NEW_PROCESS_GROUP`) | 725–743 | 19 |
| Tests (7 tests: `kill_process_tree` stale pid × 2, `classify_spawn_error` × 5) | scattered | 27 |
| **Total** | | **~160** |

All platform-specific code in `tools.rs` (`#[cfg(unix)]`, `#[cfg(windows)]`) is exclusively for the unsandboxed path. The sandboxed path uses `lot::SandboxCommand` which handles platform differences internally.

### `Cargo.toml`

```toml
# Remove entirely:
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

`libc` is used only for `setsid()` and `kill()` in the unsandboxed path. No other code in epic uses it.

### Net effect

- ~160 lines removed from `tools.rs` (9.5% of 1674 lines)
- 7 tests removed
- 1 conditional dependency removed (`libc`)
- `tokio::process::Command` removed from shell tool (remains in `orchestrator.rs` for git, unrelated)

## Simplified `tool_bash`

Current `tool_bash` has two fallback branches routing to `tool_bash_unsandboxed`. After removal:

```rust
async fn tool_bash(
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
) -> Result<BashOutput, String> {
    let command = get_str(input, "command")?.to_owned();
    let timeout_secs = input
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS)
        .min(MAX_BASH_TIMEOUT_SECS);

    let policy = build_sandbox_policy(project_root, grant)
        .map_err(|e| format!("sandbox setup failed: {e}"))?;

    tool_bash_sandboxed(command, project_root.to_path_buf(), policy, timeout_secs)
        .await
        .map_err(|e| format!("sandbox error: {e}"))
}
```

`SandboxSpawnError` and `classify_spawn_error` are no longer needed — lot errors propagate directly as `String`.

## Platform Impact

### Linux

Sandbox requires unprivileged user namespaces. Environments where this is disabled:

- **Ubuntu 24.04+**: AppArmor may restrict unprivileged user namespaces by default. Fix: `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0` or add an AppArmor profile exception.
- **Corporate lockdowns**: `kernel.unprivileged_userns_clone=0`. Fix: sysctl change (requires admin).
- **Kubernetes**: Pods with `allowPrivilegeEscalation: false` or restrictive seccomp profiles. Fix: adjust pod security policy.
- **Older kernels** (pre-3.8): No user namespace support. Unlikely on any supported distro.

Lot provides diagnostic messages for each case, including the specific sysctl or config change needed.

### Windows

AppContainer is available on Windows 10+ (epic's baseline). Failure scenarios:

- **Group Policy blocking AppContainer profile creation**: Domain-joined machines with restrictive policies. Uncommon.
- **Antivirus interfering with ACL modifications**: Some endpoint protection products. Uncommon.

### macOS

Seatbelt (`sandbox_init`) is always available on supported macOS versions. Lot reports `available() = true` unconditionally. Failure is near-impossible under normal conditions.

### GitHub Actions CI

Ubuntu-latest runners support unprivileged user namespaces and seccomp. CI uses the sandboxed path today. No impact.

## Impact Assessment

**Users affected**: Those on Linux systems with unprivileged user namespaces disabled. This is the only common failure case. macOS and Windows users are unaffected.

**User experience**: Instead of a silent warning and degraded execution, users get a clear error with lot's diagnostic message explaining what to enable. This is a one-time system configuration fix.

**Trade-off**: A small number of users on restricted Linux systems are blocked until they fix their system configuration. In exchange, epic has a simpler codebase, a single execution path, and no security-degraded mode.
