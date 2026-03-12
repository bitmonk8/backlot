# Lot Change Request: AppContainer Ancestor Traverse ACEs

## Problem

Programs running inside a lot AppContainer sandbox cannot call `fs::metadata()` on ancestor directories of policy paths. This breaks common patterns:

- NuShell's `nu_glob` decomposes absolute paths into components and calls `fs::metadata()` on each ancestor (`C:\`, `C:\Users`, etc.) during `open`, `ls <file>`, and `mkdir`.
- Rust's `std::fs::create_dir_all()` calls `fs::metadata()` on ancestors to determine which directories already exist.

`fs::metadata()` on Windows calls `CreateFileW(path, access=0, FILE_FLAG_BACKUP_SEMANTICS)`, which implicitly requires `SYNCHRONIZE`. This needs an ACE in the directory's DACL for the AppContainer SID.

Lot grants ACEs only on policy paths (with `SUB_CONTAINERS_AND_OBJECTS_INHERIT`). Ancestors of those paths have no AppContainer ACE, so `fs::metadata("C:\\")` returns `ACCESS_DENIED` → `is_dir("C:\\")` returns `false` → glob traversal produces zero results.

### Why `path exists` and `save` work but `open` and `ls <file>` don't

- `path exists` calls `fs::metadata()` on the **complete path** in a single call. The Windows kernel uses `SeChangeNotifyPrivilege` (which AppContainer processes have) to bypass traverse checks on intermediate directories. The access check is performed only on the target, which has the inherited ACE.
- `save` calls `CreateFileW(GENERIC_WRITE)` directly on the target — no glob traversal.
- `open` and `ls <file>` route through `nu_glob`, which walks ancestors one-by-one. Each ancestor is opened individually, bypassing the kernel's traverse-privilege optimization.

### Ancestor ACL survey

Checked via `icacls` on a Windows 11 machine. No ancestor directory in a typical path chain has an `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) ACE. Some have capability SIDs (`S-1-15-3-*`) from other apps, but these don't match lot's AppContainer profiles.

| Directory | Has `ALL APPLICATION PACKAGES` ACE? |
|-----------|--------------------------------------|
| `C:\` | No |
| `C:\Users` | No |
| `C:\Users\<user>` | No |
| `C:\Users\<user>\AppData` | No |
| `C:\Users\<user>\AppData\Local` | No |
| `C:\ProjectDir` | No (in general) |

Every ancestor from the volume root down to any policy path needs a traverse ACE for AppContainer sandboxes to work with programs that use glob-style path traversal.

## Proposed Change

Add a one-time elevated setup API (same pattern as `grant_nul_device_access()`) that grants minimal traverse ACEs on ancestor directories of caller-provided paths.

### API

```rust
/// One-time elevated setup. Grants all ACEs needed for AppContainer
/// sandboxes to function correctly on Windows:
///   1. NUL device read/write for ALL APPLICATION PACKAGES
///   2. Traverse ACEs on each ancestor of the provided paths, up to (and
///      including) the volume root
///
/// Idempotent — safe to call multiple times. Requires elevation.
pub fn grant_appcontainer_prerequisites(paths: &[&Path]) -> lot::Result<()>;

/// Checks whether all ancestors of each path (up to volume root) have the
/// ALL APPLICATION PACKAGES traverse ACE, and the NUL device ACE exists.
pub fn appcontainer_prerequisites_met(paths: &[&Path]) -> bool;

/// Returns true if the current process has elevation.
/// Renamed from `can_modify_nul_device()` to reflect broader scope.
pub fn can_elevate() -> bool;
```

Callers pass the paths they intend to use as policy paths (e.g., a project root directory). Lot computes the ancestors, deduplicates, and grants traverse ACEs on each.

### ACE Details

| Property | Value | Rationale |
|----------|-------|-----------|
| Access mask | `FILE_TRAVERSE \| SYNCHRONIZE` | Minimum for `fs::metadata()` to succeed |
| Inheritance | `NO_INHERITANCE` | Ancestors only — children are covered by policy path ACEs with `SUB_CONTAINERS_AND_OBJECTS_INHERIT` |
| Trustee | `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) | Same SID as the NUL device fix; covers all AppContainer profiles |
| Cleanup | Do NOT remove on sandbox teardown | System-wide prerequisites, not per-sandbox state |

Using `ALL APPLICATION PACKAGES` rather than a per-profile SID means:
- One-time grant covers all future sandboxes
- No per-spawn ACL modification on system directories
- Same approach as the existing NUL device fix

### Security Impact

- `FILE_TRAVERSE | SYNCHRONIZE` on `C:\` for `ALL APPLICATION PACKAGES` allows `fs::metadata()` to succeed, revealing that the directory exists and is a directory. AppContainer processes can already determine this via `SeChangeNotifyPrivilege` (which `path exists` uses). No new information is exposed.
- `NO_INHERITANCE` ensures the ACE does not propagate to children — existing security on `C:\Windows`, `C:\Program Files`, etc. is unaffected.
- The ACE does not grant read of directory contents (`FILE_LIST_DIRECTORY`) — only traverse and synchronize.

### Implementation Scope

1. **New public API**: `grant_appcontainer_prerequisites(paths)`, `appcontainer_prerequisites_met(paths)`, `can_elevate()`
2. **Internal: `compute_ancestors(paths) -> Vec<PathBuf>`** — for each path, walk parents up to volume root, collect into a deduplicated set
3. **Internal: `grant_traverse(path)`** — read current DACL via `GetNamedSecurityInfoW`, add `FILE_TRAVERSE | SYNCHRONIZE` ACE with `NO_INHERITANCE` for `ALL APPLICATION PACKAGES` via `SetEntriesInAclW`, apply via `SetNamedSecurityInfoW`. Same pattern as `grant_access()` but with a different access mask and no inheritance.
4. **Internal: `has_traverse_ace(path) -> bool`** — read DACL, check for an existing allow ACE for `S-1-15-2-1` with at least `FILE_TRAVERSE | SYNCHRONIZE`
5. **Fold NUL device logic into `grant_appcontainer_prerequisites()`** — the new function handles both NUL device and ancestor ACEs
6. **Deprecate** `grant_nul_device_access()`, `nul_device_accessible()`, `can_modify_nul_device()` — keep as thin wrappers calling the new API for backward compatibility
## Motivation

Epic (an AI orchestration tool) spawns NuShell inside a lot AppContainer sandbox. Nu's `open`, `ls`, and `mkdir` commands all fail because of this ancestor traversal issue. The fix in lot would benefit any lot consumer that runs programs using glob-based path resolution or `create_dir_all` inside AppContainer sandboxes.
