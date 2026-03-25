# Enhancement: Fine-Grained Path Grants

## Summary

Allow `AgentRequestConfig` to express mixed read/write access within the project root — specifically, a read-only root with a writable subdirectory.

## Motivation

Vault's librarian agent needs:

- **Read-only** access to `storage_root/` (covers `raw/`, `CHANGELOG.md`)
- **Read-write** access to `storage_root/derived/`

Reel's current `ToolGrant` applies a single permission level to the entire `project_root`. Setting `WRITE` grants write access everywhere; setting `TOOLS` makes everything read-only. There is no way to express "read the root, write this subdirectory."

## Current State

`build_nu_sandbox_policy` in `nu_session.rs` builds the lot `SandboxPolicy` from a single `project_root` + `ToolGrant`:

- `WRITE` → `write_path(project_root)`
- `TOOLS` (no WRITE) → `read_path(project_root)`

## Lot Support

Lot already supports this natively. `SandboxPolicyBuilder` accepts overlapping scopes where a write-child is under a read-parent:

```rust
SandboxPolicyBuilder::new()
    .read_path(storage_root)?        // root is read-only
    .write_path(derived_dir)?        // derived/ is read-write
    .build()?;
```

This is a tested, validated configuration (`write_child_under_read_parent_allowed` in lot's test suite). Enforcement is at the OS level (AppContainer on Windows, namespaces+seccomp on Linux, Seatbelt on macOS).

## Proposed Change

Extend `AgentRequestConfig` (or `AgentEnvironment`) to accept additional path grants beyond the project root. The exact API is open, but one approach:

```rust
pub struct AgentRequestConfig {
    pub config: flick::RequestConfig,
    pub grant: ToolGrant,
    pub custom_tools: Vec<Box<dyn ToolHandler>>,
    pub write_paths: Vec<PathBuf>,    // additional writable subdirectories
}
```

When `write_paths` is non-empty and the base grant is `TOOLS` (read-only), `build_nu_sandbox_policy` would:

1. Mount `project_root` as `read_path`
2. Mount each entry in `write_paths` as `write_path`

Lot's validation ensures each write path is a child of the read parent. Invalid configurations (write path outside project root, overlapping write paths) are rejected at build time.

## Constraints

- Must not change behavior for existing consumers that don't use `write_paths`.
- `write_paths` entries must be children of `project_root` (enforced by lot's validation).
- The base grant determines the default access level; `write_paths` elevates specific subdirectories.

## Requested By

Vault project — this enhancement is a blocking dependency for vault's librarian integration. See `vault/docs/SPEC.md > Dependencies > Reel Enhancement`.
