# Known Issues

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

### 17. No test for `rg_binary = None` branch in `spawn_nu_process`

`src/agent/nu_session.rs` — No test covers the case where `resolve_rg_binary` returns `None` (rg not present), verifying that `EPIC_RG_PATH` is correctly omitted and the session still starts. Narrow edge case. **Category: Testing.**

### 18. No test for `epic grep` nu-side `"rg"` fallback

`build.rs` (`EPIC_CONFIG_NU`) — The `epic grep` command's `else { "rg" }` branch (when `EPIC_RG_PATH` is absent) is never tested. This fallback doesn't work under AppContainer — it exists only for non-sandboxed development. **Category: Testing.**

### 19. `resolve_rg_binary` has no direct unit tests

`src/agent/nu_session.rs` — The `pub` function is tested only indirectly through integration tests. No unit test verifies resolution order (next to exe, cache dir, PATH fallback → None). **Category: Testing.**

### 20. `isolated_session()` silent fallback defeats isolation

`src/agent/nu_session.rs` — When `tmp_sandbox_cache()` returns `None` (no build-time cache), `isolated_session()` silently falls back to `NuSession::new()` which shares the default cache dir — the exact condition the helper was created to prevent. Should panic or log instead. In practice only happens when nu binary isn't built, so low risk. **Category: Testing.**

### 21. No mechanism to prevent future `NuSession::new()` in tests

`src/agent/nu_session.rs` — Nothing prevents a new test from calling `NuSession::new()` directly instead of `isolated_session()`, reintroducing the shared-directory ACL interference. A grep-based CI check or code comment convention could catch regressions. **Category: Testing.**
