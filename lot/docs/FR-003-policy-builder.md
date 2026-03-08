# FR-003: `SandboxPolicyBuilder` with Auto-Canonicalization and Defaults

## Problem

Consumers building `SandboxPolicy` manually must:

1. Canonicalize every path before inserting (lot's `validate()` rejects non-canonical paths).
2. Check for parent/child overlaps between read_paths and write_paths (lot rejects overlaps with an error).
3. Manually add `std::env::temp_dir()` to write_paths — nearly every sandboxed process needs temp dir write access.
4. Manually enumerate platform exec paths (`/usr/bin`, `/bin`, `/usr/sbin`, `/sbin`, `/usr/local/bin` on Unix; `%SYSTEMROOT%\System32` on Windows) and platform lib/include paths.

Epic currently has ~40 lines of platform-specific code (`build_sandbox_policy`, `push_canon_if_no_overlap`) doing exactly this. Any other lot consumer will write the same boilerplate.

## Proposed Solution

A builder that handles canonicalization, deduplication, and common defaults:

```rust
let policy = SandboxPolicyBuilder::new()
    .read_path("/project")              // auto-canonicalized
    .write_path("/project/src")         // auto-canonicalized, deduped against read
    .include_temp_dirs()                // adds platform temp dir to write_paths
    .include_platform_exec_paths()      // /usr/bin, System32, etc.
    .include_platform_lib_paths()       // /usr/lib, /usr/include, etc.
    .allow_network(true)
    .build()?;
```

Behavior:
- **Auto-canonicalization**: Paths are canonicalized on insert. Non-existent paths are silently skipped (or optionally error).
- **Overlap deduction**: If a path is already covered by a broader entry (parent in write_paths covers child in read_paths), the narrower entry is skipped.
- **`include_temp_dirs()`**: Adds `std::env::temp_dir()` (and platform variants like `%TEMP%`, `$TMPDIR`) to write_paths.
- **`include_platform_exec_paths()`**: Adds the standard shell/tool directories for the current platform.
- **`include_platform_lib_paths()`**: Adds library/include directories needed by compilers and build tools.

The existing `SandboxPolicy` struct and `validate()` remain unchanged — the builder produces a valid `SandboxPolicy`.

## Scope

New `policy_builder.rs` module. Platform-specific path lists in existing platform modules (or a shared `platform_paths` module).
