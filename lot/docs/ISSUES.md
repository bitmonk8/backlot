# Known Issues

Issues grouped by code area, ordered by impact.

---

## CLI (`lot-cli/`)

### CLI config types could be extracted to `config.rs`

`SandboxConfig` and sub-structs plus `build_policy` form a distinct concern from CLI dispatch. Optional; extract when the file grows larger.

**File:** `lot-cli/src/main.rs`

---

## Error Types (`error.rs`)

### `PrerequisitesNotMet` payload

The structured fields `missing_paths` and `nul_device_missing` are used in the `Display` format for diagnostics but never inspected by any match arm. A simpler `PrerequisitesNotMet(String)` with a pre-formatted message would reduce complexity at the cost of less structured diagnostic data.

**Fix:** Optional. Tradeoff between structured data and simplicity.
**File:** `lot/src/error.rs`

---

## Env/Path Module Structure (`env_check.rs`, `policy.rs`)

### `path_contains` naming ambiguity

`path_contains(parent, child)` could be read as substring containment. `any_path_contains` inherits the same ambiguity. `is_accessible_precanonicalized` embeds implementation detail in its name.

**Fix:** Rename to `is_descendant_or_equal`, `any_ancestor_of`, `is_dir_accessible` or similar.
**Files:** `lot/src/env_check.rs`

### Module name `env_check` covers general path utilities

`path_contains`, `normalize_lexical`, `canonicalize_existing_prefix` are general path utilities placed in `env_check.rs`. They have no inherent connection to environment variable checking.

**Fix:** Extract to a shared `path_util.rs` module. Have both `env_check.rs` and `policy.rs` import from it. `is_parent_of` in `policy.rs` could also delegate to the shared utility.
**Files:** `lot/src/env_check.rs`, `lot/src/policy.rs`

### `platform_implicit_read_paths` encodes platform knowledge in cross-platform module

Per-OS path lists gated by `#[cfg]` blocks belong in the respective platform modules.

**Fix:** Move lists to platform modules, expose via a dispatched function.
**Files:** `lot/src/env_check.rs`, `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`, `lot/src/windows/mod.rs`

### `canonicalize_existing_prefix` has no direct test

Non-trivial logic (iteratively popping path components, reversing, re-appending). The symlink-resolution behavior is untested.

**Fix:** Add test with a symlink where an ancestor resolves differently. Add test for full-fallback branch (no ancestor exists).
**File:** `lot/src/env_check.rs`

### `is_accessible_precanonicalized` has no direct test

Tested indirectly through integration. A unit test with pre-canonicalized arrays would isolate the logic.

**File:** `lot/src/env_check.rs`

### No test for `path_contains` with actual symlinks

The doc comment mentions `/var` → `/private/var` on macOS but no test exercises this.

**Fix:** Add macOS-specific test using `/var/tmp`.
**File:** `lot/src/env_check.rs`

### Reverse partial canonicalization (parent fails, child succeeds) untested

When `canon_parent` fails but `canon_child` succeeds, the fallback logic is untested.

**File:** `lot/src/env_check.rs`

### `kill_by_pid` on all platforms has no tests

The guard logic (rejecting PID 0, preventing self-kill) is untested. Best-effort functions, but guard correctness matters.

**Files:** `lot/src/linux/mod.rs`, `lot/src/macos/mod.rs`, `lot/src/windows/mod.rs`

