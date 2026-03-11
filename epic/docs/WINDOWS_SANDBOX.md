# Windows AppContainer: NUL Device ACL Workaround

## Background

Without a one-time ACL fix, external commands spawned by nu inside a lot AppContainer sandbox fail with `ERROR_ACCESS_DENIED` (os error 5). The error surfaces as "Command `rg` not found" because nu misreports spawn failures as command-not-found.

**Affected tool**: `epic grep` (shells out to `rg`). The other five tools (`Read`, `Write`, `Edit`, `Glob`, `NuShell`) are nu custom commands and are unaffected.

**Fix**: Run `epic setup` from an elevated (Administrator) prompt. This is a one-time operation.

## Root Cause

AppContainer blocks access to the `\\.\NUL` device. Rust's `std::process::Command` opens `\\.\NUL` when stdin is set to `Stdio::null()`. Nu's MCP mode sets `stdin(Stdio::null())` for external commands with empty input pipelines (`run_external.rs:264`). The `\\.\NUL` open fails inside AppContainer, and the error propagates as the spawn failure — `CreateProcessW` is never reached.

The relevant nu code (`nushell/crates/nu-command/src/system/run_external.rs`):

```rust
PipelineData::Empty => {
    if engine_state.is_mcp {
        command.stdin(Stdio::null());  // opens \\.\NUL → blocked by AppContainer
    } else {
        command.stdin(Stdio::inherit());
    }
    None
}
```

Nu uses `Stdio::null()` in MCP mode to prevent external commands from hanging on interactive stdin prompts. This is correct outside a sandbox but incompatible with AppContainer.

Ref: https://github.com/nushell/nushell/pull/17161#discussion_r2761243143

### Evidence

A custom test binary (`spawn_test.exe`) was run inside AppContainer via lot:

**NUL device variants — all blocked:**

| Device path | Result |
|-------------|--------|
| `\\.\NUL` | ERROR 5 |
| `NUL` | ERROR 5 |
| `\\?\NUL` | ERROR 5 |

Console devices (`\\.\CONIN$`, `\\.\CONOUT$`) succeed — they are special-cased by the kernel.

**Rust `Command` variants — only `Stdio::null()` fails:**

| Configuration | Result |
|---------------|--------|
| `Command::spawn()` (inherits stdin) | OK |
| `spawn()` + all piped | OK |
| `spawn()` + null stdin | ERROR 5 |
| `Command::output()` (defaults to null stdin) | ERROR 5 |

**Raw `CreateProcessW` — works in all configurations:**

| Configuration | Result |
|---------------|--------|
| Plain, no pipes | OK |
| With pipes + `STARTF_USESTDHANDLES` | OK |
| Full `.output()` mimic (NUL stdin + pipe stdout/stderr) | ERROR 5 at NUL open (before `CreateProcessW`) |

Conclusion: `CreateProcessW` itself is not blocked. The failure is entirely the `\\.\NUL` open.

## Solution: One-Time Elevated NUL Device ACL Grant

### Summary

Modify the DACL on `\\.\NUL` to grant `ALL APPLICATION PACKAGES` read/write access. This is a one-time operation requiring administrator elevation. The change is system-wide, persistent across reboots, and idempotent.

This is a known AppContainer limitation. Microsoft acknowledged it ([win32-app-isolation#73](https://github.com/microsoft/win32-app-isolation/issues/73)) with no built-in fix. The workaround was posted in the same issue.

### Why elevation is required

`\\.\NUL` is owned by SYSTEM. Modifying its DACL requires `WRITE_DAC`, which only elevated (administrator) processes have. A non-elevated process cannot modify the security descriptor. Windows does not support in-place process elevation — the standard pattern is to detect the need and instruct the user to re-run elevated.

### lot API (implemented, Windows-only)

Three public functions exported from `lot` crate root (`lot::nul_device_accessible`, etc.):

| Function | Signature | Behavior |
|---|---|---|
| `nul_device_accessible()` | `→ bool` | Reads `\\.\NUL` DACL via `GetNamedSecurityInfoW`, converts to SDDL, checks for an allow ACE (`(A;...;;;AC)`) for `ALL APPLICATION PACKAGES` (`S-1-15-2-1`). Returns `true` if DACL is NULL (unrestricted) or ACE exists. Returns `false` on API failure. |
| `can_modify_nul_device()` | `→ bool` | Queries `TOKEN_ELEVATION` on the current process token. Returns `true` if elevated (administrator). Returns `false` on API failure. |
| `grant_nul_device_access()` | `→ lot::Result<()>` | Idempotent — calls `nul_device_accessible()` first, returns `Ok(())` if already granted. Otherwise reads current DACL, builds `ALL APPLICATION PACKAGES` SID, adds ACE granting `FILE_GENERIC_READ \| FILE_GENERIC_WRITE` via `SetEntriesInAclW`, applies with `SetNamedSecurityInfoW`. Returns `SandboxError::Setup(msg)` on failure (including `ERROR_ACCESS_DENIED` when not elevated). |

### epic integration

**1. `epic setup` CLI subcommand**:

Checks and configures NUL device access for AppContainer sandboxing. On non-Windows, prints "Not applicable on this platform." and exits.

Behavior:
1. Calls `lot::nul_device_accessible()`. If `true` → prints "NUL device access already configured." and exits 0.
2. Calls `lot::can_modify_nul_device()`. If `false` → prints "This command must be run from an elevated (Administrator) prompt." and exits 1.
3. Calls `lot::grant_nul_device_access()`. On `Ok(())` → prints "NUL device access granted to AppContainer processes." On `Err(e)` → prints the error and exits 1.

**2. Startup check in `epic run` / `epic resume`**:

Windows-only (`#[cfg(target_os = "windows")]`). Before spawning nu sessions, calls `lot::nul_device_accessible()`. If `false`, prints an error directing the user to run `epic setup` from an elevated prompt and exits 1.

**3. Conditional compilation**:

All call sites use `#[cfg(target_os = "windows")]`. On non-Windows, the startup check compiles to a no-op and `epic setup` prints the platform message.

### Rejected alternatives

**Patch nu to use `Stdio::piped()`**: Only fixes nu's child spawning. Any process that independently opens `\\.\NUL` still fails. Does not address the root problem. Requires maintaining a nu fork or waiting for upstream acceptance.

**Temp file as null sink**: Fragile hack. Does not generalize. Still requires epic-side plumbing.

**Auto-elevation via `ShellExecuteEx("runas")`**: Opens a new console window. Bad UX for a CLI tool. Mixes concerns — epic should not manage its own privilege escalation.

## Appendix: Ruled Out Causes

- **System directory grants**: `include_platform_exec_paths()` / `include_platform_lib_paths()` were removed from the sandbox policy. AppContainer inherits System32/DLL access without explicit grants. Adding them did not fix the bug.
- **PATH corruption**: `build.rs` previously generated `epic_env.nu` with string interpolation instead of nu `prepend` for PATH. Fixed separately; was not the root cause.
- **`CreateProcessW` blocked by AppContainer**: Proven false. `CreateProcessW` succeeds in all tested configurations. The failure occurs before process creation, during the `\\.\NUL` handle open.
- **Missing execute ACLs on rg binary**: The cache directory has `FILE_GENERIC_EXECUTE` ACLs. `CreateProcessW` with the same binary succeeds when stdin is not null.
