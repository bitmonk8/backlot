# Spawn-Time Ancestor Traverse ACE Grant

## Problem

AppContainer sandboxed processes cannot call `fs::metadata()` on ancestor directories unless those directories have a traverse ACE for ALL APPLICATION PACKAGES. Without this, library code (e.g., nu_glob) that walks path components fails in opaque ways.

Most ancestor directories in a typical sandbox scenario are user-owned project directories where the current process has `WRITE_DAC`. Elevation is only needed for system directories (volume root, `C:\Users`, etc.) and the NUL device.

## Design

`spawn_inner()` grants traverse ACEs on ancestor directories between policy path collection and sentinel creation. It unconditionally ensures the ALL APPLICATION PACKAGES ACE is present on ancestors — always sufficient and never harmful.

### Algorithm

```
1. Compute ancestors of all policy paths (read_paths ∪ write_paths ∪ exec_paths)
2. For each ancestor that lacks the ALL APPLICATION PACKAGES traverse ACE:
   a. Attempt grant_traverse(path)
   b. On failure: collect into failed_paths
3. If failed_paths is non-empty OR NUL device is inaccessible:
   Return SandboxError::PrerequisitesNotMet { missing_paths, nul_device_missing }
4. Otherwise: proceed with spawn
```

### Why grant unconditionally

Gating on "does this directory already have the ACE?" would reject directories with sufficient access via other ACEs (`BUILTIN\Users`, per-user SID). Granting unconditionally adds a redundant (harmless) ACE to directories that already have sufficient access, and provides the ACE to directories that need it. The only failure mode is access denied on a directory the current user cannot modify — which genuinely requires elevated setup.

### ACE details

| Property | Value |
|----------|-------|
| Access mask | `FILE_TRAVERSE \| SYNCHRONIZE \| FILE_READ_ATTRIBUTES` |
| Inheritance | `NO_INHERITANCE` |
| Trustee | ALL APPLICATION PACKAGES (`S-1-15-2-1`) |

### Performance

~5-8 ancestors per spawn. `has_traverse_ace` does one `GetNamedSecurityInfoW` + DACL scan per ancestor. After first successful spawn, all ancestors have the ACE and the grant step becomes check-only. Cost is negligible relative to AppContainer profile creation and process launch.

### Idempotency

`grant_traverse` checks `has_traverse_ace` first. `SetEntriesInAclW` merges ACEs, so repeated grants produce identical DACLs. Safe to call on every spawn.

### Security impact

Minimal traverse permissions (`FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`), `NO_INHERITANCE`, for ALL APPLICATION PACKAGES. No new information exposed beyond what `SeChangeNotifyPrivilege` already allows.

### Consumer impact

- `spawn()` grants traverse ACEs automatically for user-owned directories.
- `grant_appcontainer_prerequisites()` (elevated) is still needed for the NUL device ACE and system directories the user cannot modify.
- The policy-based prerequisites API (`grant_appcontainer_prerequisites_for_policy`, `appcontainer_prerequisites_met_for_policy`) remains public for consumers that want explicit elevated setup.
