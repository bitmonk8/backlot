# Feature Request: NUL Device Access for AppContainer

## Problem

Processes inside an AppContainer sandbox cannot open the Windows NUL device (`\\.\NUL`). All path variants (`\\.\NUL`, `NUL`, `\\?\NUL`) return `ERROR_ACCESS_DENIED` (os error 5).

This breaks any child process that uses `Stdio::null()` — Rust's standard library opens `\\.\NUL` under the hood. NuShell's MCP mode sets `stdin(Stdio::null())` for external commands, so any external command spawned by nu inside a lot AppContainer fails before `CreateProcessW` is reached.

Known issue: [microsoft/win32-app-isolation#73](https://github.com/microsoft/win32-app-isolation/issues/73). Microsoft acknowledged it with no built-in fix.

## Solution

One-time DACL modification on the `\\.\NUL` device object to grant `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) read/write access. This is persistent across reboots and affects all AppContainer processes system-wide.

The modification requires `WRITE_DAC` on `\\.\NUL`, which is only available to elevated (administrator) processes. The NUL device is owned by SYSTEM.

## Requested API

Three new public functions, all Windows-only (`#[cfg(windows)]`):

### `nul_device_accessible() -> bool`

Test whether AppContainer processes can access `\\.\NUL`.

**Implementation approach**: Create a low-box (AppContainer) token using `CreateAppContainerProfile` (or reuse an existing profile), then call `CreateFile` on `\\.\NUL` with `GENERIC_READ` while impersonating that token. Return `true` if the open succeeds, `false` if it returns `ERROR_ACCESS_DENIED`. Clean up the token/profile after the check.

No process spawning required — this is a lightweight access check.

### `can_modify_nul_device() -> bool`

Check whether the current process has `WRITE_DAC` on `\\.\NUL`.

**Implementation approach**:
1. `CreateFile("\\.\NUL", READ_CONTROL, ...)` to get a handle.
2. `GetSecurityInfo` with `DACL_SECURITY_INFORMATION` to retrieve the security descriptor.
3. `OpenProcessToken` + `AccessCheck` with `WRITE_DAC` against the current process token.
4. Return the `AccessCheck` result.

In practice, this is equivalent to checking if the process is elevated (running as administrator), but using `AccessCheck` is more precise than role-checking.

### `grant_nul_device_access() -> Result<(), SandboxError>`

Grant `ALL APPLICATION PACKAGES` read/write access to `\\.\NUL`.

**Implementation approach**:
1. `GetNamedSecurityInfo("\\.\NUL", SE_FILE_OBJECT, DACL_SECURITY_INFORMATION)` to get the current DACL.
2. Check if an ACE for `ALL APPLICATION PACKAGES` with `GENERIC_READ | GENERIC_WRITE` already exists. If so, return `Ok(())` (idempotent).
3. `SetEntriesInAcl` to create a new DACL with an additional ACE:
   - Trustee: `ALL APPLICATION PACKAGES` (well-known SID `S-1-15-2-1`)
   - Access: `GENERIC_READ | GENERIC_WRITE`
   - Mode: `GRANT_ACCESS`
   - Inheritance: `NO_INHERITANCE`
4. `SetNamedSecurityInfo("\\.\NUL", SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, newAcl)` to apply.
5. On `ERROR_ACCESS_DENIED`, return a `SandboxError` indicating elevation is required.

**Reference implementation** (C#, from the GitHub issue):

```csharp
var strAllAppPackage = "ALL APPLICATION PACKAGES\0";
var status = Pinvoke.GetNamedSecurityInfo(
    "\\\\.\\NUL", SeObjectType.FileObject,
    SecurityInformation.Dacl,
    out _, out _, out var oldAcl, out _, out var secDesc);

var explicitAccess = new ExplicitAccess {
    grfAccessPermissions = AccessMask.GenericRead | AccessMask.GenericWrite,
    grfAccessMode = AccessMode.Grant,
    grfInheritance = InheritanceType.NoInheritance,
    Trustee = new Trustee {
        TrusteeForm = TrusteeForm.TrusteeIsName,
        TrusteeType = TrusteeType.WellKnownGroup,
        ptstrName = strAllAppPackagePtr,
    }
};

Pinvoke.SetEntriesInAcl(1, explicitAccessList, oldAcl, out newAcl);
Pinvoke.SetNamedSecurityInfo(
    "\\\\.\\NUL", SeObjectType.FileObject,
    SecurityInformation.Dacl,
    IntPtr.Zero, IntPtr.Zero, newAcl, IntPtr.Zero);
```

## Usage by epic

**At startup** (`epic run` / `epic resume`):
1. Call `lot::nul_device_accessible()`.
2. If `false` → print instructions to run `epic setup` as administrator and exit.

**`epic setup`** (new subcommand):
1. `lot::nul_device_accessible()` → if already accessible, print confirmation and exit.
2. `lot::can_modify_nul_device()` → if `false`, print "run from an elevated prompt" and exit.
3. `lot::grant_nul_device_access()` → print result.

## Scope

- Windows-only. Linux and macOS do not have this problem (`/dev/null` is accessible inside namespaces and Seatbelt sandboxes).
- Does not weaken AppContainer isolation in any meaningful way — NUL is a data sink, not a privilege escalation vector.
- The `ALL APPLICATION PACKAGES` SID covers all AppContainer processes, not just lot-spawned ones. This is acceptable because NUL access is benign.
