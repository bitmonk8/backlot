# Feature: Deny Paths

## Motivation

The sibling project `epic` needs to grant write access to a directory hierarchy while denying access to specific subtrees within it. The current policy model rejects all path overlaps, which prevents this. This document specifies a `deny_paths` extension to `SandboxPolicy`.

## Policy Model Change

Add a new field to `SandboxPolicy`:

```rust
pub struct SandboxPolicy {
    pub read_paths: Vec<PathBuf>,
    pub write_paths: Vec<PathBuf>,
    pub exec_paths: Vec<PathBuf>,
    pub deny_paths: Vec<PathBuf>,   // NEW
    pub allow_network: bool,
    pub limits: ResourceLimits,
}
```

`deny_paths` entries strip all access (read, write, execute) from matching subtrees, overriding any grants from `read_paths`, `write_paths`, or `exec_paths`.

## Semantics

- Each entry in `deny_paths` is a directory. All files and subdirectories beneath it are denied.
- A deny path must be a strict child of at least one entry in `read_paths`, `write_paths`, or `exec_paths`. A deny path with no enclosing grant is pointless (already denied by default) and should be rejected as `InvalidPolicy`.
- Deny paths must not overlap with each other (no nesting within `deny_paths`).
- Deny overrides all grant types: if `/parent` is in `write_paths` and `/parent/secret` is in `deny_paths`, the sandboxed process cannot read, write, or execute anything under `/parent/secret`.

## Validation Rules

Current overlap checks become:

1. **Intra-list overlaps**: Unchanged. No overlaps within `read_paths`, `write_paths`, `exec_paths`, or `deny_paths`.
2. **Cross-list overlaps (grant lists)**: Unchanged. The existing rules between read/write/exec lists remain.
3. **Deny vs grant**: Each deny path must be a strict child of at least one grant path. Reject if a deny path is not covered by any grant, or if a deny path equals a grant path exactly.

The existing `check_cross_overlap_directional` allowing write children under read parents remains valid and orthogonal to deny paths.

## Platform Implementation

### Linux (namespaces + bind mounts)

Do not bind-mount deny paths into the new root. Since the mount namespace starts from an empty tmpfs, paths that are not explicitly mounted do not exist. The sandboxed process cannot discover or access them.

**Mount ordering**: When a grant path is an ancestor of a deny path, mount the grant path first, then do not mount the deny path. The kernel handles this naturally — the deny subtree simply has no mount point, so it appears as an empty directory (or doesn't exist if the tmpfs has no corresponding entry).

Implementation detail: After mounting the grant parent, the deny path's mount point exists as an empty directory inside the mounted subtree. To fully hide it, `umount2` or `mount --bind /empty` over it after the parent mount. Alternatively, the current approach of selectively mounting only non-denied children achieves the same result but requires directory enumeration (same fragility as the sibling enumeration approach discussed during research).

**Recommended Linux approach**: Mount the grant parent, then overmount each deny path with an empty tmpfs:

```
bind_mount("/parent", new_root.join("parent"), READ_WRITE);
mount("tmpfs", new_root.join("parent/secret"), "tmpfs", MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC, "size=0");
```

This makes `/parent/secret` appear as an empty read-only directory. The process can see it in `readdir` but cannot read, write, or create anything inside it. Consistent with macOS/Windows behavior.

### macOS (Seatbelt SBPL)

Emit deny rules after allow rules. SBPL uses last-match-wins evaluation:

```sbpl
; Grant phase
(allow file-read* (subpath "/parent"))
(allow file-write* (subpath "/parent"))

; Deny phase (must come after grants)
(deny file-read* (subpath "/parent/secret"))
(deny file-write* (subpath "/parent/secret"))
(deny process-exec (subpath "/parent/secret"))
```

The deny rules override the earlier allows for the denied subtree. The directory entry remains visible in `readdir` of the parent, but all file operations inside it fail with EPERM.

### Windows (AppContainer ACLs + Job Objects)

Add an explicit deny ACE for the AppContainer package SID on each deny path. Windows ACL evaluation checks explicit deny before explicit allow, so the deny ACE takes precedence regardless of inherited allows from the parent.

```
SetEntriesInAcl with:
  - DENY_ACCESS for package SID on "/parent/secret"
  - Access mask: GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE
  - Inheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT
```

The deny ACE propagates to all children. The directory entry remains visible in parent listings, but all access attempts fail with ACCESS_DENIED.

**Sentinel file recovery**: Deny ACEs must be tracked in the sentinel file alongside grant ACEs, so crash recovery can remove them.

## Cross-Platform Behavior Summary

| Aspect | Linux | macOS | Windows |
|---|---|---|---|
| Denied subtree visible in parent `readdir`? | Yes (empty dir) | Yes | Yes |
| `stat()` on denied path succeeds? | Yes (empty tmpfs) | No (EPERM) | No (ACCESS_DENIED) |
| Read file inside denied path | Fails (no files exist) | Fails (EPERM) | Fails (ACCESS_DENIED) |
| Write file inside denied path | Fails (read-only tmpfs) | Fails (EPERM) | Fails (ACCESS_DENIED) |
| Create file inside denied path | Fails (read-only tmpfs) | Fails (EPERM) | Fails (ACCESS_DENIED) |

The Linux approach (empty read-only tmpfs overmount) produces slightly different error codes (ENOENT/EROFS vs EPERM/ACCESS_DENIED) but the functional result is identical: no access.

## CLI (lot-cli) Changes

Add `deny_paths` to the YAML config schema:

```yaml
read_paths:
  - /data
write_paths:
  - /data/workspace
deny_paths:
  - /data/workspace/secrets
```

## SandboxPolicyBuilder Changes

Add `deny_path()` / `deny_paths()` methods. Validation:

- Reject deny paths not covered by any grant path.
- No auto-deduplication of deny paths against grant paths (unlike write-under-read dedup). The deny is intentional and should be preserved.
- Canonicalize deny paths the same way as grant paths.

## Design Decisions

1. **`stat()` on denied paths (Linux):** Accept the cross-platform inconsistency. Linux overmount lets `stat()` succeed (empty tmpfs metadata); macOS/Windows deny it. The security guarantee ("cannot read, write, or create files inside the denied subtree") holds on all platforms. Documented, not mitigated.

2. **Full-coverage deny:** Rejected as `InvalidPolicy`. A deny path that exactly equals a grant path is a configuration error. Callers should remove the grant instead. Composing layers must reconcile grants and denies before passing the policy to lot.

3. **Nested deny paths:** Rejected as `InvalidPolicy`. A denied subtree is already fully denied recursively. Nesting is redundant and suggests a misunderstanding of the semantics. Consistent with intra-list overlap rejection for grant lists.
