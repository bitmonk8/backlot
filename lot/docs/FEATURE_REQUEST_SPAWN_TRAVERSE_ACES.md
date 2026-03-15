# Feature Request: Best-Effort Ancestor Traverse ACE Grant at Spawn Time

## Problem

Consumers of `lot::spawn()` must separately call `grant_appcontainer_prerequisites()` (elevated) to set up ancestor directory traverse ACEs before spawning. If they pass the wrong paths — or skip the call entirely — the sandboxed process starts successfully but fails in opaque ways when library code (e.g., nu_glob) walks path components via `fs::metadata()`.

The current `grant_appcontainer_prerequisites()` requires elevation, but most ancestor directories in a typical sandbox scenario are user-owned project directories where the current process already has `WRITE_DAC`. Elevation is only truly needed for system directories (volume root, `C:\Users`, etc.) and the NUL device.

The spawn-time prerequisite **check** (commit `5f71f9f`) was reverted (commit `d114feb`) because `appcontainer_prerequisites_met` only looks for ALL APPLICATION PACKAGES ACEs, producing false negatives for directories that are already traversable via other ACEs (`BUILTIN\Users`, per-user SID, etc.).

## Proposed Change

Add a best-effort traverse ACE grant step inside `spawn_inner()`, between policy path collection and sentinel creation. This does not check whether traversal already works — it unconditionally ensures the ALL APPLICATION PACKAGES ACE is present on ancestors, which is always sufficient and never harmful.

### Algorithm

```
1. Compute ancestors of all policy paths (read_paths ∪ write_paths ∪ exec_paths)
2. For each ancestor that lacks the ALL APPLICATION PACKAGES traverse ACE:
   a. Attempt grant_traverse(path)
   b. On success: continue
   c. On failure (access denied): collect into failed_paths
3. If failed_paths is non-empty OR nul_device is inaccessible:
   Return SandboxError::PrerequisitesNotMet { missing_paths: failed_paths, nul_device_missing }
4. Otherwise: proceed with spawn
```

### Why this avoids the false-negative problem

The reverted check failed because it tested "does this directory have the ACE?" as a gate. Directories with sufficient access via other ACEs were rejected.

This approach does not gate on the check. It **grants** the ACE on every ancestor that lacks it, then proceeds. Directories that already have sufficient access via other ACEs simply gain a redundant (harmless) ACE. Directories that truly need it get it. The only failure mode is access denied on a directory the current user cannot modify — which genuinely requires elevated setup.

### Where in the code

In `appcontainer.rs`, `spawn_inner()` (line ~976), after collecting `all_paths` and before `write_sentinel`:

```rust
// Best-effort: grant traverse ACEs on ancestors of policy paths.
// Succeeds without elevation for user-owned directories.
// Fails only for system directories that require elevated setup.
let ancestors = compute_ancestors_from_paths(&all_paths);
let mut failed: Vec<PathBuf> = Vec::new();
for ancestor in &ancestors {
    if !has_traverse_ace(ancestor) {
        if let Err(_) = grant_traverse(ancestor) {
            failed.push(ancestor.clone());
        }
    }
}
let nul_missing = !nul_device_accessible();
if !failed.is_empty() || nul_missing {
    return Err(SandboxError::PrerequisitesNotMet {
        missing_paths: failed,
        nul_device_missing: nul_missing,
    });
}
```

`compute_ancestors_from_paths` is a variant of `compute_ancestors` that accepts `&[PathBuf]` (the policy paths are already canonicalized by `SandboxPolicyBuilder`, so `fs::canonicalize` will succeed).

### ACE details (unchanged from existing `grant_traverse`)

| Property | Value |
|----------|-------|
| Access mask | `FILE_TRAVERSE \| SYNCHRONIZE \| FILE_READ_ATTRIBUTES` |
| Inheritance | `NO_INHERITANCE` |
| Trustee | ALL APPLICATION PACKAGES (`S-1-15-2-1`) |

### Performance

- `has_traverse_ace`: one `GetNamedSecurityInfoW` + DACL scan per ancestor
- `grant_traverse`: one `GetNamedSecurityInfoW` + `SetEntriesInAclW` + `SetNamedSecurityInfoW` per ancestor that lacks the ACE
- Typical case: ~5-8 ancestors, most already have the ACE after first spawn. Cost is negligible relative to AppContainer profile creation and process launch.
- After first successful spawn, all ancestors have the ACE and the grant step becomes check-only.

### Idempotency

`grant_traverse` already checks `has_traverse_ace` and returns early if present. `SetEntriesInAclW` merges ACEs, so repeated grants produce identical DACLs. Safe to call on every spawn.

### Security impact

Same as the existing `grant_appcontainer_prerequisites`: minimal traverse permissions (`FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`), `NO_INHERITANCE`, for ALL APPLICATION PACKAGES. No new information exposed beyond what `SeChangeNotifyPrivilege` already allows.

### What this replaces

- The reverted spawn-time check (commit `5f71f9f` / `d114feb`) — that was check-only and had false negatives. This is grant-then-check, avoiding false negatives.
- The policy-based prerequisites API (`grant_appcontainer_prerequisites_for_policy`, `appcontainer_prerequisites_met_for_policy` from commit `70017de`) can remain as a public API for consumers that want explicit elevated setup. But the common case is handled automatically at spawn time.

### Consumer impact (reel)

With this change:
- `reel run` works without `reel setup` for user-owned project directories. `lot::spawn()` grants traverse ACEs automatically.
- `reel setup` (elevated) is only needed for the NUL device ACE and for system directories the user cannot modify. Reel catches `SandboxError::PrerequisitesNotMet` and directs the user to run `reel setup` with admin privileges.
- The 3 test failures (reel_read, reel_write, reel_edit) are fixed without any reel code changes — lot handles it at spawn time.
