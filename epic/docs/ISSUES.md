# Known Issues

## CRITICAL — Nu session integration test failures

`src/agent/nu_session.rs` — 9 of 355 tests fail. These tests exercise the core tool execution path: every agent tool call routes through `NuSession`. Failures here mean epic cannot reliably execute tools inside the AppContainer sandbox on Windows.

Build and clippy are clean. 346 tests pass. Observed 2026-03-12 on Windows 11 with Rust 1.93.1.

**Status: Root causes confirmed for Categories A and B.** Category C root cause identified (concurrent profile interference), fix deferred. See investigation results below.

### Test results

Parallel run (default): 9 failures. Serialized (`--test-threads=1`): 6 failures. The 3 concurrency-only failures are marked.

| # | Test | Parallel error | Serialized error | Category |
|---|------|----------------|------------------|----------|
| 1 | `integration_custom_command_epic_read` | timeout 30s | `No matches found for DoNotExpand("...\\test.txt")` — `ls $full` fails | A |
| 2 | `integration_custom_command_epic_write` | `$env.PWD non-existent` | `Permission denied` | A |
| 3 | `integration_custom_command_epic_glob` | `$env.PWD non-existent` | **passes** | C |
| 4 | `integration_custom_command_epic_edit` | timeout 30s | timeout 30s (likely same as read) | A |
| 5 | `integration_custom_command_epic_grep` | `$env.PWD non-existent` | still fails | A/B |
| 6 | `integration_env_filtering_rg_available` | `$env.PWD non-existent` | `Command rg not found` | B |
| 7 | `integration_grant_change_respawns` | `stdout closed` | **passes** | C |
| 8 | `integration_spawn_is_idempotent` | `stdout closed` | **passes** | C |
| 9 | `integration_sandbox_read_only_prevents_writes` | `Command rg not found` | `Command rg not found` | B |

`integration_generation_prevents_stale_writeback` passes in both modes (was suspected but is not failing).

### Category A — Nu built-ins fail under AppContainer (4 tests, all fail serialized)

**Affects**: epic_read, epic_write, epic_edit, epic_grep — the four custom commands that do filesystem I/O.

#### Nu built-ins that FAIL under AppContainer

| Command | Behavior | Used by |
|---------|----------|---------|
| `open <file>` | Returns nothing (silent failure, no error) | `epic read`, `epic edit` |
| `open <file> --raw` | Returns nothing (silent failure, no error) | `epic read`, `epic edit` |
| `ls <file_path>` | `No matches found for DoNotExpand(...)` | `epic read` (size check) |
| `mkdir <dir>` | `Permission denied` (even with write ACLs) | `epic write` |

#### Nu built-ins that WORK under AppContainer

| Command | Notes |
|---------|-------|
| `save <file>` / `save --force <file>` | File creation and overwrite work |
| `ls <directory>` | Directory listing works; individual file stat does not |
| `path exists` | File existence check works |
| `path expand`, `path dirname` | Path manipulation works |
| `echo`, `lines`, `encode`, `bytes length` | String/data operations work |

#### Root cause: `nu_glob` component-by-component traversal fails on ancestor directories

**CONFIRMED via nushell source analysis (2026-03-12).**

Both `open` and `ls <file>` route through `nu_engine::glob_from()` → `nu_glob::glob_with()`. The chain:

1. `glob_from()` always converts paths to absolute (`absolute_with(path, cwd)`), so even `open test.txt` becomes `open C:\Users\...\test.txt`.
2. `nu_glob::glob_with()` decomposes the absolute path into a root (`C:\`) and component patterns (`Users`, `thomasa`, ..., `test.txt`).
3. `fill_todo()` walks components one-by-one, calling `fs::metadata()` on each intermediate directory (`C:\`, `C:\Users`, etc.).
4. `fs::metadata()` on Windows calls `CreateFileW(path, access=0, FILE_FLAG_BACKUP_SEMANTICS)`, which implicitly requires `SYNCHRONIZE` — needs an ACE in the target's DACL for the AppContainer SID.
5. `lot` only grants ACEs on policy paths (project root, temp dirs, cache dir) with `SUB_CONTAINERS_AND_OBJECTS_INHERIT`. It does NOT grant ACEs on `C:\`, `C:\Users`, or other ancestor directories outside the policy paths.
6. `fs::metadata("C:\\")` fails with `ACCESS_DENIED` → `is_dir("C:\\")` returns `false` → `fill_todo` adds nothing to the iterator → zero results.
7. `open` returns `PipelineData::empty()` (nushell `nothing` type — silent). `ls` checks `paths_peek.peek().is_none()` and returns `ShellError::GenericError("No matches found")`.

**Why `path exists` works:** Calls `fs::metadata()` on the **complete path** at once. Windows kernel uses `SeChangeNotifyPrivilege` (which AppContainer processes have) to bypass traverse checks on intermediate directories. The access check is performed only on the target object, which has the inherited AppContainer ACE.

**Why `save` works:** Does not use `nu_glob` at all — directly calls `CreateFileW(GENERIC_WRITE)` on the target path.

**Why relative paths fail:** `glob_from()` converts them to absolute before passing to `nu_glob`.

**`mkdir` fails independently** — `std::fs::create_dir_all()` also calls `fs::metadata()` on ancestors to check what exists, triggering the same volume-root access failure.

#### Eliminated hypotheses

- ~~ACL inheritance failure~~ — `icacls` confirms inherited `(I)(RX,W)` for `ALL APPLICATION PACKAGES`
- ~~Path canonicalization mismatch~~ — No junctions/symlinks/8.3 names in paths
- ~~NUL device ACL~~ — Configured and working
- ~~`%TEMP%`-specific~~ — Same failures in `target/test-step3/` and inside sandbox
- ~~Custom command issue~~ — Raw `open` and `ls <file>` fail identically outside custom commands
- ~~Relative paths bypass volume root~~ — `glob_from()` converts relative→absolute, so all paths traverse the volume root

#### Resolution options

Three fix options, from least to most invasive:

1. **Grant traverse ACEs on ancestor directories in lot** — Add read-only ACEs for the AppContainer SID on each ancestor directory from the policy path up to (and including) the volume root. This lets `fs::metadata()` succeed on `C:\`, `C:\Users`, etc. Minimally invasive to epic. Trade-off: reveals directory structure to sandboxed process (acceptable — AppContainer already allows `path exists` on any path via `SeChangeNotifyPrivilege`).

2. **Patch `nu_glob` to use `GetFileAttributesW` instead of `CreateFileW`** — `GetFileAttributesW` does not open a handle and may not require the same DACL ACE. Or patch `nu_glob` to catch `ACCESS_DENIED` on intermediate directories and continue traversal (treat inaccessible ancestor as traversable if the final target is accessible). Trade-off: upstream patch, maintenance burden.

3. **Move file I/O to Rust** — Execute `Read`, `Write`, `Edit` in the epic Rust process instead of routing through nu. `NuShell`, `Glob`, `Grep` remain in nu. Trade-off: changes security model (file I/O no longer sandboxed).

### Category B — `Command rg not found` (2 tests, both fail serialized)

**Affects**: env_filtering_rg_available, sandbox_read_only_prevents_writes — any test that executes `^rg` inside the sandbox.

**Root cause: CONFIRMED.** No `rg.exe` binary exists on this machine. The `rg` command available in bash is a Claude Code shell function that proxies to `claude.exe` — it is not a standalone executable. The nu session's `resolve_rg_binary()` checks three locations and all fail:
1. Same directory as current executable — no `rg.exe` there
2. Cache directory (`NU_CACHE_DIR`) — compile-time `option_env!()` macro, not set at build time
3. Bare `rg.exe` on `PATH` — does not exist (`where.exe rg` confirms)

NUL device is not a factor (see Step 1 in investigation results). The test assertion message in `integration_sandbox_read_only_prevents_writes` misleadingly suggests running `epic setup` — that message needs updating.

#### Resolution options

This is a test environment issue, not a sandbox bug. Options: (a) `build.rs` downloads rg alongside nu (it already does for nu), (b) tests skip when rg is unavailable, (c) install rg on the dev machine.

### Category C — Concurrency-only failures (3 tests, pass serialized)

**Affects**: epic_glob, spawn_is_idempotent, grant_change_respawns.

These pass with `--test-threads=1` but fail under parallel execution:

- **spawn_is_idempotent** and **grant_change_respawns**: `nu process closed stdout unexpectedly` — concurrent AppContainer profile create/destroy races.
- **epic_glob**: Transient PWD resolution failure under concurrent profile management.

Root cause: AppContainer profiles created by concurrent test processes interfere. The sandbox tests use per-test isolated cache dirs, but these non-sandbox tests share the build-time cache dir.

### Investigation results

Steps 1–6 completed 2026-03-12. Key findings incorporated into Category A, B, and C sections above. Summary: (1) NUL device ACL configured — not a factor in any category. (2) ACL inheritance correct, no path mismatch. (3) Not `%TEMP%`-specific. (4) 19 isolated tests confirmed which nu built-ins fail vs work. (5) Relative paths fail identically (glob_from converts to absolute). (6) Traced to `nu_glob::fill_todo()` → `fs::metadata()` on ancestor directories.

**Remaining: Address concurrency (Category C).** PENDING — deferred until Category A fix is verified.

---

## Non-critical issues

### 1. `run_structured` ToolCallsPending branch is untested

`src/agent/flick.rs` — `run_structured` bails if the model returns `ToolCallsPending` (hallucinated tool calls in a structured-only context). No test exercises this branch. A `SingleShotProvider` returning `ToolCallsPending` status would cover it.

### 2. `FlickAgent::new()` error paths untested

`src/agent/flick.rs` — `FlickAgent::new()` can fail in two ways: `build_model_registry()` and `ProviderRegistry::load_default()`. Neither error path is tested — `with_injected` bypasses both. These are thin wrappers with straightforward error mapping, so the risk is low. Consider adding a `new_with_registries()` constructor or accepting an optional `ProviderRegistry` for testability if these paths grow more complex.

### 3. Config JSON round-trip in `build_config`

`src/agent/config_gen.rs` — `build_config` constructs a `serde_json::Value`, serializes to string, then passes to `RequestConfig::from_str` which re-parses. If flick exposes `from_value` or a builder API, the round-trip is eliminable. Check next time flick is updated.

### 4. Missing wire-type edge-case tests

`src/agent/config_gen.rs` — Several conversion error paths lack test coverage:
- `VerificationWire` with `outcome: "fail"` (both with and without `reason`)
- `parse_model_name` with invalid input (e.g., `"gpt4"`)
- `TaskOutcomeWire` with invalid outcome (e.g., `"partial"`)
- `DetectedStepWire` conversion: default timeout (300) when `timeout` is `None`
- `SubtaskWire` with invalid magnitude (e.g., `"huge"`)

### 5. `run_with_tools` resume timeout untested

`src/agent/flick.rs` — The timeout test only covers the initial `client.run()`. No test covers timeout during `client.resume()` in the tool loop. A `SlowProvider` that responds quickly on first call (with tool calls) but slowly on resume would cover this.

### 6. Timeout/error-mapping pattern duplication

`src/agent/flick.rs` — The `tokio::time::timeout(...).await.map_err(...)` pattern appears three times with near-identical structure. A small `timed_call` helper would deduplicate. Low urgency — cosmetic.

### 7. `model_key()` and `default_max_tokens()` placement

`src/agent/config_gen.rs` — Both functions encode model-tier policy (tier → registry key, tier → token budget) but live in config_gen.rs, whose stated purpose is "in-memory config, wire format types, output schemas." Their primary consumer is `build_model_registry()` in flick.rs. Move them to flick.rs or a shared module. **Category: Placement.**

### 8. `extract_text` mutable loop

`src/agent/flick.rs` — `extract_text` iterates all content blocks with a mutable `last_text` variable. `iter().rev().find_map(...)` is more direct. **Category: Simplification.**

### 9. Deprecated `TempDir::into_path()` warning

`src/agent/tools.rs:1247` — Uses `TempDir::new().unwrap().into_path()` which triggers a deprecation warning: `use of deprecated method tempfile::TempDir::into_path: use TempDir::keep()`. Replace with `TempDir::keep()` when convenient.
