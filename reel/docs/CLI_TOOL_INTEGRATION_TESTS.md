# Integration Test Failures: reel_read, reel_write, reel_edit

## Failing tests

- `integration_custom_command_reel_read` — "No matches found" from `nu_glob`
- `integration_custom_command_reel_write` — "Already exists" from `save`
- `integration_custom_command_reel_edit` — "Input type not supported: nothing" from `open`

## Passing tests

- `integration_custom_command_reel_glob` — passes
- `integration_custom_command_reel_grep` — passes

## Root cause

All three failures trace to missing Windows AppContainer prerequisites — specifically the NUL device ACL and ancestor traverse ACEs.

### What the prerequisites are

`lot::grant_appcontainer_prerequisites(&[project_root])` does two things:

1. **NUL device ACL**: Grants `ALL APPLICATION PACKAGES` (S-1-15-2-1) read/write access to `\\.\NUL`. Without this, nu's internal operations that open NUL (stderr redirection for child processes, internal pipeline operations) fail inside AppContainer.
2. **Ancestor traverse ACEs**: Grants `FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE` on every ancestor directory from the project root up to the volume root. Without this, nu's `open`/`save` built-ins fail because `nu_glob` calls `fs::metadata()` on each ancestor.

### How epic's tests worked

Epic's custom command integration tests (in `src/agent/nu_session.rs`, now deleted) did **not** invoke the `epic` CLI binary. They called `NuSession::evaluate()` directly — the same API-level approach reel's tests use now. Reel's tests are a mechanical rename of epic's tests (command prefix `epic` → `reel`, sandbox base dir `epic-sandbox-test` → `reel-sandbox-test`, identical test infrastructure and skip logic).

The tests relied on a prior `epic setup` run having granted the global NUL device ACL and ancestor traverse ACEs. The skip-on-failure mechanism (`try_spawn`/`try_eval` returning `None` when the error contains `"sandbox setup failed"`) only catches sandbox policy build failures — it does not catch the runtime failures caused by missing AppContainer ACLs. So the tests assume prerequisites are met; they do not skip gracefully when they're not.

This means reel's tests should ultimately work the same way: call `NuSession::evaluate()` directly (as they already do), but with a proper prerequisite check so they skip when ACLs are not configured rather than failing with opaque errors.

### Why reel's tests fail

`reel setup` now exists in `reel-cli` with `--check` and `--verbose` flags, calling `lot::grant_appcontainer_prerequisites()` directly. However, reel does not yet re-export lot's prerequisite functions via a `reel::sandbox` module (item 1 below), and there is no prerequisite check in the test infrastructure (item 3 below). The tests proceed, the sandbox spawn succeeds (the `"sandbox setup failed"` check only catches policy build failures, not missing ACLs), and then nu commands fail at runtime with opaque errors.

### Why glob and grep pass

- **`reel glob`**: Uses nu's `glob` built-in — purely directory enumeration within nu's process, using the read path ACL that the sandbox policy sets up dynamically per-spawn. No child process spawning, no NUL device access.
- **`reel grep`**: Spawns `rg` via `| complete` which captures all streams. Either `rg` handles missing NUL gracefully, or `| complete` avoids the NUL stdin path that normal external command invocation uses.

### Why read/write/edit fail

All three use nu's `open` and/or `save` built-ins, which route through `nu_glob` for path resolution. `nu_glob` calls `fs::metadata()` on each ancestor directory. Without the traverse ACEs, these calls return `ACCESS_DENIED`. The resulting errors vary by command:

- `reel read`: `open` triggers `nu_glob` → ancestor traversal fails → "No matches found"
- `reel write`: `save` may partially succeed or encounter stale state → "Already exists"
- `reel edit`: `open` fails silently returning nothing → `str replace` receives `nothing` instead of string → "Input type not supported"

## Fix

Three things needed:

### 1. Reel should re-export lot's prerequisite functions

Epic currently calls `lot::appcontainer_prerequisites_met()`, `lot::is_elevated()`, and `lot::grant_appcontainer_prerequisites()` directly. These exist because reel's sandbox needs them — lot is an implementation detail of reel. Epic (and any other reel consumer) should not depend on lot directly for this.

Reel should re-export these functions from `lib.rs` (behind a `#[cfg(windows)]` gate):

```rust
// Re-export sandbox prerequisite functions for consumers.
// Consumers need these for CLI setup commands and pre-flight checks,
// but should not depend on lot directly — lot is reel's implementation detail.
#[cfg(windows)]
pub mod sandbox {
    pub use lot::{appcontainer_prerequisites_met, grant_appcontainer_prerequisites, is_elevated};
}
```

Then epic replaces `lot::appcontainer_prerequisites_met(...)` with `reel::sandbox::appcontainer_prerequisites_met(...)` etc., and removes its direct `lot` dependency from `Cargo.toml`.

### 2. Add `reel setup` to `reel-cli`

Mirror epic's `run_setup()` — call `reel::sandbox::grant_appcontainer_prerequisites(&[project_root])` from an elevated prompt.

### 3. Add a prerequisite check to the test infrastructure

Skip sandbox tests when prerequisites are not met. Options:
- Add a `skip_no_acl!()` macro that checks `reel::sandbox::appcontainer_prerequisites_met(&[sandbox_test_base()])`
- Add the check to `sandbox_env()` and return `None` from `try_spawn`/`try_eval`
- Either way, the tests should skip gracefully rather than fail with opaque errors

### Workaround

Running `epic setup` from `C:\UnitySrc\epic` grants the global NUL device ACL and ancestor traverse ACEs for `C:\UnitySrc` and `C:\`, which also covers reel's test paths. This works but creates a hidden dependency on epic.
