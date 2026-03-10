# Windows Sandbox Setup

Epic sandboxes AI agent processes using [lot](https://github.com/bitmonk8/lot), which uses Windows AppContainer on Windows. AppContainer requires ACL modifications on directories the sandboxed process needs to access. This document explains the setup.

## The Problem

When lot creates an AppContainer sandbox, it calls `SetNamedSecurityInfoW` to grant the AppContainer SID access to each directory in the sandbox policy. This Win32 call requires `WRITE_DAC` permission on the target directory.

Epic's sandbox policy includes these directories:

| Directory | Source | Purpose |
|-----------|--------|---------|
| Project root | epic | Read or read-write access to project files |
| `%TEMP%` | `include_temp_dirs()` | Scratch space for agent work |
| `%SYSTEMROOT%\System32` | `include_platform_exec_paths()` | Access to system executables (cmd.exe, etc.) |
| `%ProgramFiles%` | `include_platform_lib_paths()` | Access to installed libraries |
| `%ProgramFiles(x86)%` | `include_platform_lib_paths()` | Access to 32-bit installed libraries |
| `target\nu-cache\` | epic | Access to nu binary, config files, and rg binary |

The user typically owns the project root, temp dir, and nu-cache dir, so `WRITE_DAC` is already available there. The system directories (`System32`, `ProgramFiles`) are owned by `TrustedInstaller` or `Administrators`, so `SetNamedSecurityInfoW` fails with `Access is denied (error 5)`.

## Setup (One-Time, Elevated)

Grant your user `WRITE_DAC` on the system directories. Open PowerShell **as Administrator** and run:

```powershell
icacls "$env:SYSTEMROOT\System32" /grant "$env:USERNAME:(WDAC)"
icacls "$env:ProgramFiles" /grant "$env:USERNAME:(WDAC)"
icacls "${env:ProgramFiles(x86)}" /grant "$env:USERNAME:(WDAC)"
```

This grants `WRITE_DAC` on the top-level directory objects only (not recursively). Lot uses `SUB_CONTAINERS_AND_OBJECTS_INHERIT` when setting the AppContainer ACE, so the top-level grant is sufficient.

After this, epic runs without elevation.

### Verifying the Setup

From a normal (non-elevated) shell:

```
cargo test integration_sandbox -- --nocapture
```

All three `integration_sandbox_*` tests should pass. If they fail with "sandbox setup failed: grant ACLs: Access is denied", the `WRITE_DAC` grants did not take effect — recheck the `icacls` commands.

### Reverting the Setup

To remove the grants:

```powershell
icacls "$env:SYSTEMROOT\System32" /remove "$env:USERNAME"
icacls "$env:ProgramFiles" /remove "$env:USERNAME"
icacls "${env:ProgramFiles(x86)}" /remove "$env:USERNAME"
```

## Security Implications

`WRITE_DAC` lets your user modify the ACL (not the contents) of the directory object. This is narrower than full control:

- It does **not** grant read/write/execute on files within the directory.
- It does **not** apply recursively (no `/T` flag).
- It allows your user to add or remove ACEs on the directory itself.

The risk: a process running as your user could modify the directory's ACL to grant itself broader access. This is a minor privilege escalation vector if your account is already compromised. For most development environments this is acceptable.

If this is not acceptable for your environment, run epic from an elevated shell instead.

## For Epic Developers

### Running Sandbox Integration Tests

The `integration_sandbox_*` tests in `src/agent/nu_session.rs` call `session.evaluate()` directly — they do not silently skip on sandbox failure. If the sandbox cannot be set up, the tests fail. This is intentional: these tests verify security-critical behavior and must not produce false positives.

To run them:
1. Complete the one-time setup above, OR
2. Run `cargo test` from an elevated shell.

### Platform Notes

| Platform | Mechanism | Setup Required |
|----------|-----------|----------------|
| Windows | AppContainer + ACLs | Yes — `WRITE_DAC` grants (this document) |
| Linux | User namespaces + seccomp | No — works if unprivileged user namespaces are enabled (kernel default) |
| macOS | Seatbelt (SBPL) | No — always available |

### Future Improvement

The `WRITE_DAC` requirement is a limitation of lot's current implementation. Lot could avoid this by:
- Using `SE_RESTORE_PRIVILEGE` token adjustment (available to Administrators without per-path grants)
- Skipping ACL grants on system directories that AppContainer profiles already have implicit read/execute access to
- Providing an `epic setup` command that performs the one-time elevated ACL grants automatically

These changes would live in the lot crate, not in epic.
