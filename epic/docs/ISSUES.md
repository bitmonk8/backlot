# Known Issues

## CRITICAL — Nu session integration test failures

`src/agent/nu_session.rs` — 9 of 355 tests fail. These tests exercise the core tool execution path: every agent tool call routes through `NuSession`. Failures here mean epic cannot reliably execute tools inside the AppContainer sandbox on Windows.

Build and clippy are clean. 346 tests pass. Observed 2026-03-12 on Windows 11 with Rust 1.93.1.

**Status: Root cause diagnosis required before fixes.** See investigation plan below.

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

### Category A — AppContainer blocks file access (4 tests, all fail serialized)

**Affects**: epic_read, epic_write, epic_edit, epic_grep — the four custom commands that do filesystem I/O.

Serialized execution reveals the real errors (parallel runs mask them with stale-PWD and timeout noise):

- **epic_read**: `No matches found for DoNotExpand("C:\\...\\Temp\\.tmpTdKVcR\\test.txt")` — nu's `ls $full` cannot see the file despite it existing on disk.
- **epic_write**: `Permission denied` — `save` cannot write despite `ToolGrant::WRITE` and `include_temp_dirs()`.
- **epic_edit**: Timeout (30s) — likely the same access failure as read, but nu hangs instead of erroring.
- **epic_grep**: Fails with rg/PWD errors — compounds with Category B.

These tests use `tmp_project()` → `TempDir::new()` (in `%TEMP%`) with `NuSession::new()` (shared cache dir). The sandbox has `write_path(%TEMP%)` via `include_temp_dirs()` and `exec_path(cache_dir)`.

**Hypothesized root cause**: AppContainer ACLs are applied to the directory, but files created by the *parent* process (outside the sandbox) before the child spawns may not inherit the ACL. Files pre-created by the test are invisible to the sandboxed nu process. The sandbox tests (`sandbox_env()`) work differently — they use `tmp_sandbox_project()` (outside `%TEMP%`) with `write_path` on the specific directory.

**This is unverified.** It could also be: ACL canonicalization mismatch (junction/symlink in `%TEMP%` path), or a lot bug in `write_path` inheritance propagation.

### Category B — `Command rg not found` (2 tests, both fail serialized)

**Affects**: env_filtering_rg_available, sandbox_read_only_prevents_writes — any test that executes `^rg` inside the sandbox.

- **`integration_env_filtering_rg_available`**: Uses shared cache dir. `resolve_rg_binary()` finds rg.exe in cache, sets `EPIC_RG_DIR`, `epic_env.nu` prepends to `$env.Path`. AppContainer may block execution due to missing ACLs on the shared cache dir.

- **`integration_sandbox_read_only_prevents_writes`**: Uses isolated cache dir copy with `exec_path(cache_dir)`. Possible causes: (1) `std::fs::copy` doesn't preserve ACLs on the copied rg.exe, (2) AppContainer blocks child process spawning without an explicit ACE for `ALL APPLICATION PACKAGES`, (3) the NUL device ACL is not configured — `epic setup` must be run from an elevated prompt first.

**Unresolved question**: Has `epic setup` been run on this machine? If not, *all* external commands fail in AppContainer because nu opens `\\.\NUL` for stdin piping. This would explain both Category B tests and possibly contribute to Category A failures.

### Category C — Concurrency-only failures (3 tests, pass serialized)

**Affects**: epic_glob, spawn_is_idempotent, grant_change_respawns.

These pass with `--test-threads=1` but fail under parallel execution:

- **spawn_is_idempotent** and **grant_change_respawns**: `nu process closed stdout unexpectedly` — concurrent AppContainer profile create/destroy races.
- **epic_glob**: Transient PWD resolution failure under concurrent profile management.

Root cause: AppContainer profiles created by concurrent test processes interfere. The sandbox tests use per-test isolated cache dirs, but these non-sandbox tests share the build-time cache dir.

### Investigation plan

Each step depends on findings from the previous step. Do not skip ahead.

**Step 1 — Establish NUL device state.**
Run `epic setup` status check (or `lot::nul_device_accessible()` in a test) to determine if the NUL device ACL is configured. If not, run `epic setup` from an elevated prompt and re-run all tests. If Category B tests pass after this, the rg issue is resolved and Category A can be investigated in isolation.

**Step 2 — Isolate ACL inheritance vs path mismatch.**
Write a minimal reproducer: create a file in `%TEMP%` outside the sandbox, then try to read it from inside AppContainer with `write_path(%TEMP%)`. If it fails, the issue is in lot's ACL inheritance (lot bug). If it passes, the issue is in epic's test setup or path handling.

**Step 3 — Check path canonicalization.**
Print `std::fs::canonicalize(std::env::temp_dir())` and compare with the actual `TempDir::new()` path. If they differ (e.g., junction resolution), the ACL is being applied to a different path than the one nu receives.

**Step 4 — Test `sandbox_env()` for custom commands.**
Temporarily change the failing custom command tests to use `sandbox_env()` (project root outside `%TEMP%`). If they pass, the issue is specific to `%TEMP%` path handling and `include_temp_dirs()`.

**Step 5 — Address concurrency.**
If Steps 1-4 resolve Category A and B, add `#[serial]` to the 3 Category C tests, or switch all integration tests to use isolated cache dirs.

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
