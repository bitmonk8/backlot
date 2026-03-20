# Documentation Spec

## Target State

Publish-ready documentation following Rust crate conventions: README.md for discovery (crates.io / GitHub), rustdoc for API reference (docs.rs), examples/ for runnable code.

### README.md

The README serves as the landing page on crates.io and GitHub. It should contain:

- One-paragraph summary of what lot does.
- Badge row: crates.io version, docs.rs link, CI status, license.
- Platform mechanisms table (already exists).
- Quick-start example (minimal spawn with policy).
- Feature flags table.
- Link to docs.rs for full API reference (not a duplicate of it).
- CLI section (`lot run`, `lot setup`, `lot probe`) — keep, this is useful context.
- Windows prerequisites section — keep.
- Known limitations — keep.
- License.

The current README duplicates API reference material that belongs in rustdoc. After the migration, the README should link to docs.rs instead of inlining full API docs.

### Rustdoc

Crate-level docs in `lib.rs` (`//!` block) should contain:

- Summary paragraph (exists).
- Platform table (exists).
- **Quick-start example** — a compilable `# fn main()` example showing `SandboxPolicyBuilder` → `SandboxCommand` → `spawn()` → `wait_with_output()`. This appears on the docs.rs front page.
- Key functions list (exists).
- Feature flags section.
- Link to the README for CLI usage / prerequisites.

Per-item docs (`///`) on all public types, functions, and methods:

| Item | Current Status |
|---|---|
| `SandboxCommand` + methods | Documented |
| `SandboxStdio` | Documented |
| `SandboxError` | Documented |
| `SandboxPolicy` + methods | Documented |
| `ResourceLimits` | Documented |
| `SandboxPolicyBuilder` + methods | Documented |
| `PlatformCapabilities` | Documented |
| `SandboxedChild` + methods | Documented |
| `probe()` | Documented |
| `spawn()` | Documented |
| `cleanup_stale()` | Documented |
| `platform_implicit_read_paths()` | Documented |
| `is_elevated()` (Windows re-export) | **Missing** |
| `appcontainer_prerequisites_met()` (Windows re-export) | **Missing** |
| `grant_appcontainer_prerequisites()` (Windows re-export) | **Missing** |
| `grant_appcontainer_prerequisites_for_policy()` | Documented |
| `appcontainer_prerequisites_met_for_policy()` | Documented |

Doc comments should include:

- One-line summary.
- Behavior description where non-obvious.
- Platform-specific notes where applicable (e.g., `cleanup_stale` is Windows-only in effect).
- `# Errors` section on fallible functions listing which `SandboxError` variants can be returned.
- `# Examples` section with compilable doctests on key entry points (`spawn`, `probe`, `SandboxPolicyBuilder::build`).
- `# Panics` section where applicable.

### examples/

Create `examples/` directory with runnable examples:

| File | Purpose |
|---|---|
| `basic.rs` | Minimal sandbox: build policy, spawn `echo hello`, read output. |
| `deny_paths.rs` | Demonstrate deny-path carve-outs from granted paths. |
| `resource_limits.rs` | Demonstrate memory/process/CPU limits. |

Examples must compile on all platforms (gate platform-specific paths behind `#[cfg]` or use `platform_implicit_read_paths()`). Each example should have a module-level doc comment explaining what it demonstrates.

### Cargo.toml metadata

Add missing fields to `lot/Cargo.toml` `[package]`:

| Field | Value |
|---|---|
| `repository` | GitHub URL (when public) |
| `documentation` | `https://docs.rs/lot` |
| `readme` | `../README.md` |
| `keywords` | `["sandbox", "seccomp", "appcontainer", "seatbelt", "isolation"]` |
| `categories` | `["os", "api-bindings"]` |

### include_str! unification

Use `#![doc = include_str!("../../README.md")]` in `lib.rs` — **do not adopt**. The README contains CLI docs, prerequisites, and badges that don't belong in rustdoc. Keep crate-level docs and README as separate documents with different audiences.

## Changes Required

### 1. Add doc comments to Windows re-exports

In `lib.rs`, add `///` doc comments to `is_elevated()`, `appcontainer_prerequisites_met()`, and `grant_appcontainer_prerequisites()` at their re-export site.

### 2. Add `# Examples` doctests to key items

Add compilable examples to:
- `spawn()` — show full policy → command → spawn → wait flow.
- `probe()` — show capability detection.
- `SandboxPolicyBuilder` — show builder pattern usage.
- `SandboxedChild::wait_with_output()` — show output capture.

Doctests should use `no_run` attribute (they require OS mechanisms that may not be available).

### 3. Add `# Errors` sections

Add `# Errors` to all public functions that return `Result`: `spawn`, `probe`, `cleanup_stale`, `SandboxPolicy::validate`, `SandboxPolicyBuilder::build`, the Windows prerequisite functions.

### 4. Create examples/ directory

Add `basic.rs`, `deny_paths.rs`, `resource_limits.rs` as described above.

### 5. Update Cargo.toml metadata

Add `repository`, `documentation`, `readme`, `keywords`, `categories`.

### 6. Slim down README.md

Remove the inline API reference section (types, methods, builder pattern details). Replace with a quick-start example and a link to docs.rs. Keep CLI docs, prerequisites, known limitations.

### 7. Add quick-start example to lib.rs crate docs

Add a compilable `no_run` example in the `//!` block showing the basic flow.
