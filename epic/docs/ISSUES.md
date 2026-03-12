# Known Issues

## Nu session integration test failures

`src/agent/nu_session.rs` — 3 of 45 nu_session tests fail (serialized), 9 in parallel. These tests exercise the core tool execution path: every agent tool call routes through `NuSession`.

Build and clippy are clean. 42 nu_session tests pass (serialized). Observed 2026-03-12 on Windows 11 with Rust 1.93.1.

**Status: Category A RESOLVED (2026-03-12). Category B RESOLVED (2026-03-12).** Lot ancestor ACEs + epic per-session temp dir fix verified. Remaining: Category C (parallel interference, 6 tests).

### Test results

Parallel run (default): 6 failures. Serialized (`--test-threads=1`): 0 failures. The 6 concurrency-only failures are marked.

| # | Test | Parallel | Serialized | Category |
|---|------|----------|------------|----------|
| 1 | `integration_custom_command_epic_read` | **passes** | **passes** | A (resolved) |
| 2 | `integration_custom_command_epic_write` | **passes** | **passes** | A (resolved) |
| 3 | `integration_custom_command_epic_edit` | **passes** | **passes** | A (resolved) |
| 4 | `integration_custom_command_epic_glob` | **passes** | **passes** | A (resolved) |
| 5 | `integration_custom_command_epic_grep` | **passes** | **passes** | B (resolved) |
| 6 | `integration_env_filtering_rg_available` | **passes** | **passes** | B (resolved) |
| 7 | `integration_sandbox_read_only_prevents_writes` | **passes** | **passes** | B (resolved) |
| 8 | `integration_spawn_creates_session` | `stdout closed` | **passes** | C |
| 9 | `integration_evaluate_simple_echo` | `stdout closed` | **passes** | C |
| 10 | `integration_evaluate_multiple_sequential` | `stdout closed` | **passes** | C |
| 11 | `integration_drop_cleans_up` | `stdout closed` | **passes** | C |
| 12 | `integration_timeout_kills_process` | `stdout closed` | **passes** | C |
| 13 | `integration_grant_change_respawns` | `stdout closed` | **passes** | C |

### Category A — Nu built-ins fail under AppContainer (RESOLVED — all 4 pass)

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

#### Ancestor ACL survey (2026-03-12)

`icacls` confirms no ancestor directory has an `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) ACE. The capability SIDs (`S-1-15-3-*`) present on `C:\`, `C:\Users`, `C:\Users\thomasa` are from other apps and do not match lot's AppContainer profiles. `C:\Users\thomasa\AppData` and `...\AppData\Local` have no AppContainer ACEs at all. Every ancestor in every policy path chain needs a traverse ACE.

Full survey table and proposed fix were in `lot/docs/CHANGE_REQUEST_FOR_EPIC.md` (lot repo). The fix is now implemented.

#### Resolution: lot change (done) + epic temp dir redirect (done)

Two changes work together to fix Category A:

**1. Lot: ancestor traverse ACEs — DONE (rev `8b468d7`)**

Lot now provides `grant_appcontainer_prerequisites(paths)` which grants `FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE` ACEs on all ancestor directories up to the volume root, plus NUL device access. Epic's `epic setup` and startup check updated to use the new API. Running `epic setup` from an elevated prompt will apply these ACEs for the project root.

**2. Epic: per-session temp directory under `.epic/` — DONE (2026-03-12)**

Each nu session now gets its own temp directory under `<project_root>/.epic/tmp/` via `tempfile::TempDir`. Changes:
- `spawn_nu_process()` creates `.epic/tmp/` base, creates a `TempDir` inside it
- `TEMP`/`TMP` env vars set before `forward_common_env()` (explicit env takes precedence)
- `build_nu_sandbox_policy()` uses `write_path(session_temp_dir)` instead of `include_temp_dirs()`
- `NuProcess` holds the `TempDir` handle — auto-cleaned on drop
- Lot policy validation updated to allow write-path children under read-path parents
- `tempfile` moved from dev-dependencies to dependencies
- Project root existence validated before temp dir creation

**Verified (2026-03-12).** `epic setup` applied ancestor traverse ACEs. All 4 Category A tests pass serialized. Category A is resolved.

### Category B — `Command rg not found` (RESOLVED — all 3 pass)

**Affected**: custom_command_epic_grep, env_filtering_rg_available, sandbox_read_only_prevents_writes — tests that executed rg inside the sandbox.

**Background**: `build.rs` downloaded rg to `target/nu-cache/` and `resolve_rg_binary()` found it correctly. The binary was present on disk.

**Root cause**: NuShell's PATH-based external command lookup (`^rg`, `which rg`) fails under AppContainer on Windows. `forward_common_env()` forwards `PATH` as a single semicolon-joined string. Nu stores it as one list element and does not split semicolons for executable search. Even after `epic_env.nu` prepended the rg dir, `which rg` returned `[]` despite the file existing at that location.

**Resolution (2026-03-12):**
- `spawn_nu_process()` sets `EPIC_RG_PATH` env var with the full absolute path to the rg binary.
- `epic_config.nu`: `epic grep` uses `^$env.EPIC_RG_PATH` to invoke rg by absolute path, bypassing nu's broken PATH lookup.
- `EPIC_RG_DIR` and the PATH-prepend block in `epic_env.nu` removed (dead code after this fix).
- Tests updated to use `^$env.EPIC_RG_PATH` instead of bare `^rg`.

### Category C — Concurrency-only failures (6 tests, all pass serialized)

**Affects**: spawn_creates_session, evaluate_simple_echo, evaluate_multiple_sequential, drop_cleans_up, timeout_kills_process, grant_change_respawns.

These pass with `--test-threads=1` but fail under parallel execution with `nu process closed stdout unexpectedly`.

Root cause: AppContainer profiles created by concurrent test processes interfere. The sandbox tests use per-test isolated cache dirs, but these non-sandbox tests share the build-time cache dir. Now unblocked for investigation (Category A resolved).

### Investigation results

Steps 1–6 completed 2026-03-12. Key findings incorporated into Category A, B, and C sections above. Summary: (1) NUL device ACL configured — not a factor in any category. (2) ACL inheritance correct, no path mismatch. (3) Not `%TEMP%`-specific. (4) 19 isolated tests confirmed which nu built-ins fail vs work. (5) Relative paths fail identically (glob_from converts to absolute). (6) Traced to `nu_glob::fill_todo()` → `fs::metadata()` on ancestor directories.

**Remaining:** Category C (parallel AppContainer interference). Categories A and B resolved.

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

### 10. `lot` dependency uses local path override

`Cargo.toml` — `lot = { path = "../lot" }` is a local dev override. Must revert to a pinned git rev before merge. Blocked on committing the lot policy.rs changes (directional overlap validation) to the lot repo first.

### 11. Per-session temp dir test gaps

`src/agent/nu_session.rs` — Missing test cases for the per-session temp dir feature:
- No integration test verifying nu sees the overridden `TEMP`/`TMP` env vars (e.g., `$env.TEMP` should point under `.epic/tmp/`).
- No positive test that a read-only session can write to its per-session temp dir (complement to `integration_sandbox_temp_dir_no_pivot_to_project`).
- No test verifying temp dir cleanup on `NuProcess` drop (though `tempfile::TempDir` cleanup is well-tested upstream).
- No unit test for `spawn_nu_process` with a nonexistent `project_root` (new validation path).
- No policy test asserts absence of system temp dirs from `write_paths` — a regression re-adding `include_temp_dirs()` would go undetected.

### 12. Policy test boilerplate duplication

`src/agent/nu_session.rs:606-689` — 5 `build_nu_sandbox_policy` tests repeat identical `TempDir` + `TempDir::new_in` setup. Extract a helper like `fn policy_test_dirs() -> (TempDir, TempDir)`. **Category: Simplification.**

### 13. Double `.join()` in temp base path construction

`src/agent/nu_session.rs:494` — `.join(".epic").join("tmp")` can be `.join(".epic/tmp")`. **Category: Simplification.**

### 14. `NuProcess::drop` uses unbounded `wait()` after `kill()`

`src/agent/nu_session.rs:95` — `child.wait()` in the `Drop` impl uses `WaitForSingleObject(INFINITE)`. If `kill()` fails silently (e.g., `ERROR_ACCESS_DENIED` during process exit), `wait()` could block the thread indefinitely. Practical likelihood is very low on Windows (`TerminateProcess` is reliable), but a bounded wait or `try_wait` fallback would be more defensive. Drop impl also relies on struct field declaration order for the kill→wait→TempDir-drop sequence; an explicit `drop()` call would make the dependency clearer. **Category: Correctness (edge case).**

### 15. `spawn_nu_process` responsibilities exceed name

`src/agent/nu_session.rs:480` — Function now handles 6 concerns: project_root validation, temp dir creation, sandbox policy building, binary resolution, process spawning, MCP handshake. The MCP handshake could be extracted into a method on `NuProcess`. Defer to reel extraction when this code moves to its own crate. **Category: Naming / Separation of Concerns.**

### 16. Tests assume `EPIC_RG_PATH` is always set

`src/agent/nu_session.rs` — Two tests use `^$env.EPIC_RG_PATH` directly without guarding for its absence. If `resolve_rg_binary` ever returns a non-absolute path (the PATH fallback case), `EPIC_RG_PATH` won't be set and tests will fail with an opaque nu error. In practice, `build.rs` always provides the cached absolute path. Add a guard or `try_eval`-style skip if this becomes fragile. **Category: Testing.**

### 17. ~~`resolve_rg_binary` validation at call site~~ (RESOLVED)

Resolved 2026-03-12: `resolve_rg_binary` now returns `Option<PathBuf>`, validating `is_absolute() && exists()` internally. `spawn_nu_process` consumes the `Option` directly.

### 18. No test for `rg_binary = None` branch in `spawn_nu_process`

`src/agent/nu_session.rs` — No test covers the case where `resolve_rg_binary` returns `None` (rg not present), verifying that `EPIC_RG_PATH` is correctly omitted and the session still starts. Narrow edge case. **Category: Testing.**

### 19. No test for `epic grep` nu-side `"rg"` fallback

`build.rs` (`EPIC_CONFIG_NU`) — The `epic grep` command's `else { "rg" }` branch (when `EPIC_RG_PATH` is absent) is never tested. This fallback doesn't work under AppContainer — it exists only for non-sandboxed development. **Category: Testing.**

### 20. `resolve_rg_binary` has no direct unit tests

`src/agent/nu_session.rs` — The `pub` function is tested only indirectly through integration tests. No unit test verifies resolution order (next to exe, cache dir, PATH fallback → None). **Category: Testing.**
