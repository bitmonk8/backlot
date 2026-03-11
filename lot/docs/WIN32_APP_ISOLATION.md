# Win32 App Isolation — Relevance to Lot

## What It Is

Win32 App Isolation is a Windows 11 (24H2+, build 26100+) security feature that builds on AppContainer to provide a higher-level sandboxing model for Win32 apps. It adds:

- **Brokering File System (BFS)**: A kernel mini-filter driver (`bfs.sys`) that mediates file access between the isolated app and the real filesystem. BFS opens files on behalf of the isolated app using its own privileges, bypassing normal ACL checks.
- **File/Registry Virtualization**: Writes to `%LocalAppData%` and `%AppData%` are automatically redirected to a per-app virtual store. The app sees a merged view of real and virtualized files.
- **Capability Manifest**: Access is declared via named capabilities in an MSIX `AppxManifest.xml` instead of manual DACL programming.
- **Application Capability Profiler (ACP)**: A development-time tool that traces access-denied events and outputs the exact capability declarations needed.

Apps run as low-integrity AppContainer processes. Child processes inherit the isolation context (same AppContainer SID, same BFS policies).

## Why It Matters for Lot

Lot's Windows backend currently uses raw AppContainer + manual ACL management. This is the most complex part of the codebase (~1,566 lines in `appcontainer.rs`) due to:

1. **ACL grant/revoke**: Before launch, lot grants the AppContainer SID read or read-write ACEs on every allowed path via `SetNamedSecurityInfo`. On cleanup, it restores original DACLs from saved SDDL strings.
2. **Sentinel file recovery**: To handle crashes, lot writes a sentinel file containing original SDDLs before modifying any ACLs. On next `spawn()`, stale sentinels trigger ACL restoration. This is fragile (temp dir races, lost sentinels orphan ACLs permanently).
3. **SID and handle lifecycle**: Manual allocation/freeing of SIDs, DACLs, security descriptors across multiple error paths.

Win32 App Isolation could eliminate all of this. BFS brokers file access without modifying filesystem DACLs, so there is no ACL state to manage or recover. The key architectural insight:

- A generic launcher exe is packaged as an isolated MSIX app.
- Lot registers the MSIX package once (not per-spawn).
- The launcher inherits the AppSilo context and spawns the target process, which inherits the same isolation.
- Filesystem access is controlled by BFS policies rather than DACL manipulation.
- Cleanup requires only BFS policy removal and MSIX unregistration — no DACL restoration, no sentinel files.

## Why We Can't Use It Yet

### No arbitrary path grants in the manifest

The capability model provides only predefined access categories:

| Capability | Scope |
|---|---|
| `isolatedWin32-accessToPublisherDirectory` | `C:\ProgramData\*<publisherID>` only |
| `isolatedWin32-promptForAccess` | Interactive user prompt (not headless) |
| `isolatedWin32-volumeRootMinimal` | System DLL loading |
| `broadFileSystemAccess` | Wide access, requires MS Store review |

Lot needs to grant access to arbitrary caller-specified paths (e.g., `C:\Users\me\project\src`). No manifest capability supports this.

### BFS policy API is undocumented

BFS policies can be set programmatically via IOCTLs (`0x228004` for SetPolicy, `0x228010` for DeletePolicy) from a Medium IL process. This would allow lot to authorize arbitrary paths without ACL modification. However, these IOCTLs were discovered via CVE reverse engineering, not official documentation. Using them in a library is not viable.

### Other blockers

- **Windows 11 24H2+ only** — drops Windows 10 and older Windows 11 support.
- **MSIX registration latency** — package registration takes seconds, not milliseconds. Requires a pre-registration lifecycle rather than simple `spawn()`.
- **Signing requirement** — self-signed packages need a one-time admin cert trust operation. Unsigned packages with executables require admin per-install.
- **Still in preview** — Microsoft explicitly disclaims API stability.

## What Would Unblock Adoption

Any of the following would make Win32 App Isolation viable for lot:

1. **Documented BFS policy API** for programmatic path authorization from a broker process.
2. **Manifest support for arbitrary path declarations** (e.g., a capability that accepts path parameters).
3. **A programmatic API to grant file access to an AppSilo process** without modifying filesystem DACLs (documented equivalent of the BFS IOCTLs).

## Reference Material

### Microsoft documentation
- [App Isolation overview](https://learn.microsoft.com/en-us/windows/win32/secauthz/app-isolation-overview)
- [Supported capabilities](https://learn.microsoft.com/en-us/windows/win32/secauthz/app-isolation-supported-capabilities)
- [App consent model](https://learn.microsoft.com/en-us/windows/win32/secauthz/app-isolation-app-consent)
- [MSIX packaging for App Isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/app-isolation-msix-packaging)
- [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation)
- [Application security — App Isolation](https://learn.microsoft.com/en-us/windows/security/book/application-security-application-isolation)
- [MakeAppx.exe tool](https://learn.microsoft.com/en-us/windows/msix/package/create-app-package-with-makeappx-tool)
- [Manual MSIX packaging](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-manual-conversion)
- [Unsigned MSIX packages](https://learn.microsoft.com/en-us/windows/msix/package/unsigned-package)
- [App Isolation release notes](https://learn.microsoft.com/en-us/windows/win32/secauthz/app-isolation-release-notes)

### Developer blogs
- [Public preview: Win32 app security via App Isolation (2023)](https://blogs.windows.com/windowsdeveloper/2023/06/14/public-preview-improve-win32-app-security-via-app-isolation/)
- [Sandboxing Python with Win32 App Isolation (2024)](https://blogs.windows.com/windowsdeveloper/2024/03/06/sandboxing-python-with-win32-app-isolation/)

### GitHub
- [microsoft/win32-app-isolation](https://github.com/microsoft/win32-app-isolation) — samples, ACP tool, consent documentation
- [Issue #36 — Flexible virtualization](https://github.com/microsoft/win32-app-isolation/issues/36) — request for broader write virtualization
- [Issue #40 — AppSilo directory listing](https://github.com/microsoft/win32-app-isolation/issues/40)

### BFS internals (reverse engineering)
- [CVE-2025-29970 BFS vulnerability analysis — PixiePoint Security](https://www.pixiepointsecurity.com/blog/nday-cve-2025-29970/) — documents BFS IOCTL interface, PolicyTable structure, and access control model
- [BFS January 2025 patch analysis — ht3labs](https://ht3labs.com/Brokering-File-System-January-2025-Patch-Analysis.html) — additional BFS internal details

### WinRT APIs (for future programmatic use)
- [PackageManager.AddPackageByUriAsync](https://learn.microsoft.com/en-us/uwp/api/windows.management.deployment.packagemanager.addpackagebyuriasync?view=winrt-26100)
- [PackageManager in windows-rs](https://microsoft.github.io/windows-docs-rs/doc/windows/Management/Deployment/struct.PackageManager.html)
