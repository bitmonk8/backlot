# Known Issues

## Non-critical issues

### 1. No tests for `ToolHandler` dispatch

`reel/src/agent.rs` — No tests cover the custom tool dispatch path: name matching against model tool calls, priority over built-ins, unknown tool error handling. Add when implementing first consumer (epic's Research Service). **Category: Testing.**

### 2. No integration tests for full tool execution path

`reel/src/tools.rs`, `reel/src/nu_session.rs` — Unit tests exist for `quote_nu()`, grant checks, sandbox policy, and response parsing. No end-to-end test validates `execute_tool()` → NuSession command translation → subprocess execution → result parsing. **Category: Testing.**

### 3. Items migrated from epic

The following issues were originally tracked in epic's `docs/ISSUES.md` and moved here during the reel extraction. Line numbers refer to reel's copies of the files.

#### 3a. `NuProcess::drop` uses unbounded `wait()` after `kill()`

`child.wait()` in `Drop` uses `WaitForSingleObject(INFINITE)`. If `kill()` fails silently, `wait()` blocks indefinitely. Practical likelihood very low. Drop impl relies on struct field declaration order for kill→wait→TempDir-drop. **Category: Correctness (edge case).**

#### 3b. Per-session temp dir test gaps

Missing tests: nu seeing overridden `TEMP`/`TMP` env vars, read-only session writing to temp dir, temp dir cleanup on drop, `spawn_nu_process` with nonexistent `project_root`, policy test asserting absence of system temp dirs from `write_paths`. **Category: Testing.**

#### 3c. Policy test boilerplate duplication

5 `build_nu_sandbox_policy` tests repeat identical `TempDir` + `TempDir::new_in` setup. Extract a helper. **Category: Simplification.**

#### 3d. Tests assume `REEL_RG_PATH` is always set

Two tests use `^$env.REEL_RG_PATH` without guarding for absence. **Category: Testing.**

#### 3e. No test for `rg_binary = None` branch

No test covers `resolve_rg_binary` returning `None`. **Category: Testing.**

#### 3f. `resolve_rg_binary` has no direct unit tests

Tested only indirectly through integration tests. **Category: Testing.**

#### 3g. `isolated_session()` silent fallback defeats isolation

Falls back to `NuSession::new()` when `tmp_sandbox_cache()` returns `None`. Should panic instead. **Category: Testing.**

#### 3h. No mechanism to prevent `NuSession::new()` in tests

Nothing prevents tests from using `NuSession::new()` directly instead of `isolated_session()`. **Category: Testing.**

### ~~4. `lot` dependency uses local path override~~ (FIXED)

Fixed: replaced with `git = "https://github.com/bitmonk8/lot", rev = "8b468d7"`. **Category: Build.**

### 5. `Agent::run()` dispatch heuristic uses `ToolGrant::NU` instead of tool availability

`reel/src/agent.rs` — `run()` decides between structured and tool-loop mode based on `ToolGrant::NU`. A consumer with only custom tools (no NU grant) would be routed to structured mode incorrectly. No such consumer exists yet. **Category: Correctness.**

### 6. No test for timeout during resume (tool loop) phase

`reel/src/agent.rs` — Timeout during `client.resume()` in the tool loop is not tested. The structured-mode timeout test covers the pattern. **Category: Testing.**

### 7. No test for project root change triggering NuSession respawn

`reel/src/nu_session.rs` — `evaluate_inner` restarts the nu process when project root changes. Grant-change respawn test covers the mechanism but not this specific trigger. **Category: Testing.**

### 8. `NuSession` re-exported but no external consumer

`reel/src/lib.rs` — `pub use nu_session::NuSession` is part of the public API but no consumer uses it directly. Consider removing after API stabilization. **Category: Placement.**

### ~~9. Pre-existing test failures~~ (PARTIALLY FIXED)

#### ~~9a. Sandbox policy path overlap (5 tests)~~ (FIXED)

Fixed by lot rev `f131ad9` which allows write-path children under read-path parents.

#### ~~9b. Nu custom commands not loaded~~ (FIXED)

Fixed: removed obsolete `--string` flag from `reel_config.nu` for nu 0.111.0.

#### 9c. Nu built-in file commands fail inside AppContainer sandbox (3 tests)

**Root cause identified: nu 0.111.0 built-in commands `ls`, `open`, and `mkdir` fail when the nu process is spawned inside a Windows AppContainer via `lot::spawn`. The same commands work when nu is spawned directly via `std::process::Command` (no AppContainer). Nu's `ls` uses the `nu_glob` crate for file matching; nu's `glob` command uses the `wax` crate. Under AppContainer, `nu_glob` fails while `wax` succeeds for identical patterns. The `open` command silently returns `nothing` instead of file content. The exact Win32 API calls that AppContainer restricts have not been identified.**

This is NOT an issue in nu's MCP mode itself. When nu `--mcp` is spawned directly via `std::process::Command` (no AppContainer), all commands work correctly.

##### Verified behavior inside AppContainer (lot::spawn)

**Commands that FAIL:**
- `ls` (no args) — fails: "No matches found for Expand(`*`)"
- `ls <file-path>` (absolute or relative) — fails: "No matches found for DoNotExpand(`...`)"
- `ls *.txt`, `ls **/*.txt` — fails: "No matches found"
- `open <file> --raw` — returns `nothing` (null). No error. `describe` reports type `nothing`.
- `open <file>` (no --raw, any extension: .txt, .json, .csv) — returns `null` or `[]`
- `open <file> --raw | decode utf-8` — fails: "Pipeline empty" (nothing to decode)
- `open <file> --raw | bytes length` — fails: "Pipeline empty"
- `mkdir <existing-dir>` — fails: "Already exists"

**Commands that WORK inside AppContainer:**
- `ls <dir-path>` — lists directory contents
- `ls .` — lists cwd contents
- `glob '*'`, `glob <exact-path>` — finds files (uses `wax` crate, not `nu_glob`)
- `path exists` — returns `true` for existing files
- `path type` — returns `"file"` correctly
- `path expand` — returns correct path
- `save --force <file>` — writes file content successfully
- `pwd`, `cd` — work correctly
- External commands via `^` — work (e.g., `rg` via `REEL_RG_PATH`)

**All of the above work correctly when nu is spawned without AppContainer** (tested via `std::process::Command` with identical MCP protocol).

##### Two distinct failure modes

1. **`ls` glob resolution fails (`nu_glob` crate)**: Nu's `ls` command uses the `nu_glob` crate for pattern matching (even for literal filenames). The `glob` command uses the `wax` crate. Under AppContainer, `nu_glob`'s file enumeration fails while `wax`'s succeeds for the same patterns. The exact Win32 API difference is not yet identified. `ls <dir>` and `ls .` work — they use a different code path than `ls <file>` or `ls <glob-pattern>`.

2. **`open` returns `nothing`**: The `open` command finds the file (no "not found" error) but produces no value into the pipeline. `save` (file write) works, and `path exists` confirms the file exists — so ACLs allow the file to be found, but `open` fails silently. The exact mechanism is not yet identified — no errors appear on stderr.

##### Evidence

- **Without AppContainer** (nu spawned via `std::process::Command`): `open --raw | describe` returns `"byte stream"`, `ls file.txt` shows metadata, `mkdir existing` succeeds silently, `let x = (open file --raw); $x == null` returns `false`. All commands work.
- **With AppContainer** (nu spawned via `lot::spawn`): Same commands fail as described above. `open` on nonexistent file errors with "File not found"; `open` on existing file returns `nothing`. `let x = (open file --raw); $x == null` returns `true`. Files created by `save` in the same session are also unreadable by `open`.
- **`ls <dir> | where name ends-with 'test.txt'`** works in AppContainer — the file IS visible in directory listings, just not through `nu_glob` pattern matching.
- No errors on nu's stderr for the `open` case — it silently returns nothing.
- The env var `$env.TEMP` inside AppContainer shows `C:\Users\...\Packages\lot-...\AC\Temp` — Windows redirects TEMP for AppContainer processes at the OS level, regardless of explicit env var overrides.

##### Why the earlier diagnosis was wrong

Early investigation used `isolated_session()` / `tmp_project()` as a "no sandbox" control, but `NuSession` always spawns via `lot::spawn` with a `SandboxPolicy`. There is no unsandboxed mode in `NuSession`. The correct control test was spawning nu directly via `std::process::Command` without lot.

##### These tests did not exist in epic

The 3 failing tests (`integration_custom_command_reel_read/write/edit`) were created during the reel extraction. Epic has no equivalent tests. Epic's STATUS.md references reel's failures.

##### No known upstream issue

Searched nushell GitHub issues (2026-03-14) — no reports of `nu_glob` failing inside AppContainer or `open` returning nothing under sandboxed processes.

##### Affected custom commands

- `reel read` — uses `ls $full` (fails via `nu_glob`) and `open $full --raw` (returns nothing)
- `reel write` — uses `mkdir $parent` (fails on existing dir)
- `reel edit` — uses `open $full --raw` (returns nothing), causing downstream `split row` type error

##### Unaffected custom commands

- `reel glob` — uses `cd $dir; glob $pattern` (`wax` crate works in AppContainer)
- `reel grep` — shells out to external `rg` via `^$rg_cmd` (external commands work)

##### Diagnostic tests

Diagnostic tests are in `reel/src/nu_session.rs`, prefixed with `diag_`. Run with:
```
cargo test -p reel diag_ -- --nocapture
```

Key tests:
- `diag_nu_mcp_without_lot` — proves all commands work without AppContainer
- `diag_raw_ls_in_sandbox` — shows `ls <file>` fails but `ls <dir>` works in AppContainer
- `diag_glob_vs_ls_appcontainer` — shows `glob '*'` works but `ls` (same pattern) fails
- `diag_bytestream_vs_string` — shows `open` returns `nothing` in AppContainer

**Category: AppContainer / nu_glob interaction.**

### 10. `extract_text` uses mutable loop instead of iterator

`reel/src/agent.rs` — `extract_text` iterates forward with a `let mut last_text` variable. Replace with `result.content.iter().rev().find_map(...)`. **Category: Simplification.**

### 11. `dispatch_tool` linear scan on custom tools

`reel/src/agent.rs` — `dispatch_tool` calls `handler.definition()` on every custom tool handler for every tool call, just to compare names. Build a `HashMap<&str, usize>` once at the start of `run_with_tools`. Low urgency — custom tool count will be 0–1 near term. **Category: Simplification.**

### 12. `parse_config` and `emit_error` untested in reel-cli

`reel-cli/src/main.rs` — `parse_config` does a two-pass YAML parse-strip-reparse and has no test coverage — most likely regression point. `emit_error` output shape is part of the CLI's documented interface. **Category: Testing.**

### 13. `RunResult` field propagation untested

`reel/src/agent.rs` — `usage` and `response_hash` mapping from `FlickResult` is untested in both `run_structured` and `run_with_tools` paths. All mock providers use `UsageResponse::default()`. Need tests with non-default usage/hash values. **Category: Testing.**

### 14. `ToolCallsPending` in structured mode — test deleted, not replaced

`reel/src/agent.rs` — `run_structured` bails when the model hallucinates tool calls. Epic previously tested this; the test was deleted during extraction but not recreated in reel. **Category: Testing.**

### 15. Multi-tool-call-per-round counting untested

`reel/src/agent.rs` — `total_tool_calls += tool_calls.len() as u32` accumulates across rounds. No test verifies correct counting when a single round returns multiple tool calls. **Category: Testing.**

### 16. reel-cli two-pass YAML config parse over-engineered

`reel-cli/src/main.rs` — `parse_config` parses YAML three times (once for `ReelFields`, once as generic map, re-serialized and parsed again by flick). Simplify: parse once as `serde_yml::Value`, pop `grant` key, pass remainder. **Category: Simplification.**

### 17. reel-cli Windows setup functions duplicate cwd setup

`reel-cli/src/main.rs` — `check_windows_prerequisites` and `configure_windows_prerequisites` both do `current_dir()` → `vec![cwd.as_path()]`. Extract a helper. **Category: Simplification.**

### 18. `build_effective_config` is a trivial wrapper

`reel/src/agent.rs` — One-line public method that calls `Self::build_request_config(request)`. The indirection adds no value. **Category: Simplification.**

### 19. reel-cli calls lot directly instead of through reel re-exports

`reel-cli/Cargo.toml` — `reel-cli` depends on `lot` directly and calls `lot::appcontainer_prerequisites_met` / `lot::grant_appcontainer_prerequisites`. Per `CLI_TOOL_INTEGRATION_TESTS.md`, reel should re-export these via a `reel::sandbox` module so consumers don't depend on lot directly. **Category: Correctness.**

### 20. `evaluate_inner` holds lock during nu process spawn

`reel/src/nu_session.rs` — `evaluate_inner` awaits `spawn_nu_process` while holding the async `Mutex`, blocking `kill()` if spawn hangs. Should spawn outside the lock. **Category: Concurrency.**

### 21. `spawn()` does not verify grant/project_root match

`reel/src/nu_session.rs` — `spawn()` returns early if `process.is_some()` without checking whether the existing process matches the requested parameters. **Category: Correctness.**

### 22. Nu sandbox allows network unconditionally

`reel/src/nu_session.rs` — `build_nu_sandbox_policy` sets `.allow_network(true)` regardless of grant level. A model-crafted NuShell command could exfiltrate data. **Category: Security.**

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

### 24. `MAX_TOOL_ROUNDS` caps rounds, not individual tool calls

`reel/src/agent.rs` — A model sending 5 tool calls per round can execute 250+ calls within 50 rounds. Consider adding a per-session tool call cap. **Category: Design.**

### 25. `WRITE` without `NU` is accepted but meaningless

`reel/src/agent.rs` — `ToolGrant::WRITE` without `ToolGrant::NU` produces empty tool list and routes to structured mode. No write capability. **Category: API clarity.**

### 26. Built-in file tools have fixed 120s timeout

`reel/src/tools.rs` — Only the NuShell tool respects model-provided timeout. File tools (Read, Write, Edit, Glob, Grep) use 120s. Slow Grep on large codebases cannot be extended. **Category: Feature gap.**

### 27. `build_request_config` reconstructs config via accessors — may lose future fields

`reel/src/agent.rs` — Reads individual fields from `request.config` via getters and rebuilds a new `RequestConfig`. If `flick::RequestConfig` gains fields, they will be silently dropped. A clone-and-mutate approach would be more forward-compatible. **Category: Fragility.**

### 28. `reel glob` has no depth limit or symlink protection

`reel/build.rs` (REEL_CONFIG_NU) — `reel glob` runs `glob $pattern` with a 1000-result cap but no depth limit. A `**/*` pattern in a deep tree with symlink cycles could hang before the cap is reached. **Category: Robustness.**

### 29. `NuSession` leaves `.reel/tmp/` directory in project root

`reel/src/nu_session.rs` — Creates `<project_root>/.reel/tmp/` for tempdir. The tempdir itself is cleaned up, but the `.reel/tmp/` parent directory is left behind. Should use system temp dir or clean up the parent. **Category: Side effects.**

### 30. `ToolGrant::from_names` returns `Result<_, String>` instead of a proper error type

`reel/src/tools.rs` — Library crate should define error types for better consumer error handling. **Category: API design.**

### 31. `test_support` re-exports leak flick internal types through reel's public API

`reel/src/lib.rs` — When `testing` feature is enabled, types like `SingleShotProvider`, `DynProvider`, etc. are re-exported from flick. Changes to flick's test types break reel's API without any reel code changing. **Category: Semver hazard.**

### 32. `NU_CACHE_DIR` is baked in at compile time

`reel/src/nu_session.rs` — `option_env!("NU_CACHE_DIR")` is resolved at build time. If the binary is relocated, config files path goes stale and `resolve_config_files()` returns `None`, causing nu to start without custom commands (tool calls fail). Binary fallback works but config does not. **Category: Portability.**

### 33. reel-cli blocking stdin read on single-threaded async runtime

`reel-cli/src/main.rs` — `std::io::stdin().read_to_string()` blocks the `current_thread` tokio runtime. Works in practice since it runs before any async work, but should use `spawn_blocking` or async IO. **Category: Correctness.**

### 34. reel-cli `--timeout 0` accepted without validation

`reel-cli/src/main.rs` — No lower bound on timeout value. `Duration::from_secs(0)` causes instant timeout on every operation. **Category: Validation.**

### 35. reel-cli dry run output omits grant info and uses different JSON format

`reel-cli/src/main.rs` — Dry run uses `to_string_pretty` and omits the resolved `ToolGrant`; success output uses `to_string` (compact). Inconsistent format and missing diagnostic info. **Category: Usability.**
