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

**Root cause: missing ALL APPLICATION PACKAGES traverse ACEs on intermediate directories between the volume root and the sandbox policy paths.** nu_glob walks path components from root to leaf, calling `fs::metadata()` at each level. When an intermediate directory lacks the traverse ACE, `fs::metadata()` returns "Permission denied" and nu_glob produces zero matches.

##### Root cause mechanism

nu 0.111.0's `ls` and `open` commands resolve file paths through `glob_from()` → `nu_glob::glob_with()`. The nu_glob crate's `fill_todo` function (`crates/nu-glob/src/lib.rs:977-996`) walks path components **one at a time from the filesystem root**. For non-glob patterns (literal filenames), it calls `fs::metadata(&next_path)` at each intermediate directory to verify it exists before descending.

Inside a Windows AppContainer, `fs::metadata()` requires `FILE_READ_ATTRIBUTES` permission on the target path. Directories that lack an ALL APPLICATION PACKAGES (`S-1-15-2-1`) traverse ACE (which includes `FILE_READ_ATTRIBUTES`) return "Permission denied". nu_glob treats this as "path doesn't exist" and produces an empty iterator.

In contrast, nu's `glob` command uses the `wax` crate → `walkdir` → `std::fs::read_dir()`, which enumerates the parent directory via `FindFirstFileW`/`FindNextFileW`. This works because the AppContainer has access to the granted leaf directory directly, without needing to traverse intermediates one at a time.

Similarly, `path exists` calls `fs::metadata()` on the **full final path** in a single call. The AppContainer allows this for paths within the grant because the per-profile ACE on the policy path (with `SUB_CONTAINERS_AND_OBJECTS_INHERIT`) covers the full path. Only the component-by-component walk triggers the intermediate directory check.

##### How lot's ACL system works (two tiers)

1. **Per-spawn** (`grant_acls` in `appcontainer.rs:1385`): Grants the ephemeral per-profile AppContainer SID on the exact policy paths (read/write/exec). Runs at normal privilege. Does NOT grant on ancestors.

2. **Prerequisites** (`grant_appcontainer_prerequisites` in `nul_device.rs:617`): Grants ALL APPLICATION PACKAGES traverse ACEs (`FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE`) on every ancestor directory of the input paths, plus NUL device access. Requires elevation. Designed as one-time machine setup.

`compute_ancestors` (`nul_device.rs:352-377`) walks `parent()` up to the volume root. It excludes the input paths themselves — only ancestors.

##### The gap

`reel setup` (`reel-cli/src/main.rs:276`) passes only `cwd` to `grant_appcontainer_prerequisites`. This grants traverse ACEs on ancestors of cwd. But the test sandbox paths are **children** of cwd (e.g., `<cwd>/reel/target/sandbox-test/.tmpXXX`). The intermediate directories between cwd and the sandbox path (`reel/`, `reel/target/`, `reel/target/sandbox-test/`) receive NO traverse ACEs because they are descendants of cwd, not ancestors.

##### Evidence: intermediate directory metadata failure

Diagnostic test `diag_intermediate_dir_metadata` probes `path exists` and `path type` (which calls `fs::symlink_metadata`) on every path component from root to the test file inside the AppContainer:

| Path component | `path exists` | `path type` |
|---|---|---|
| `C:\` | true | dir |
| `C:\UnitySrc` | true | dir |
| `C:\UnitySrc\reel` | true | dir |
| `C:\UnitySrc\reel\reel` | **false** | **Permission denied** |
| `C:\UnitySrc\reel\reel\target` | **false** | **Permission denied** |
| `C:\UnitySrc\reel\reel\target\sandbox-test` | **false** | **Permission denied** |
| `...\sandbox-test\.tmpXXX` (granted dir) | true | dir |
| `...\sandbox-test\.tmpXXX\probe.txt` | true | file |

`C:\` and `C:\UnitySrc` have ALL APPLICATION PACKAGES traverse ACEs (from a prior `reel setup` run). `C:\UnitySrc\reel` has inherited `BUILTIN\Users:(RX)` which provides sufficient access. `C:\UnitySrc\reel\reel` through `sandbox-test` have neither — the AppContainer cannot read their attributes.

##### Confirmed fix

Manually granting ALL APPLICATION PACKAGES traverse ACEs on the intermediate directories (`C:\UnitySrc\reel\reel`, `reel\target`, `reel\target\sandbox-test`) causes all 3 tests to pass. This was verified by running `grant_appcontainer_prerequisites` with paths deep enough to cover the test sandbox directory.

##### How `open` fails silently

`open` uses the same `glob_from()` → `nu_glob` path as `ls` (`crates/nu-command/src/filesystem/open.rs:110-126`). When nu_glob returns zero matches (due to the intermediate directory permission failure), the `output` vec is empty. At line 258-259, `open` returns `PipelineData::empty()` — no error, no data, just nothing.

##### Affected nu commands and code paths

| Command | Crate | Function | Fails because |
|---|---|---|---|
| `ls <file>` | nu_glob | `fill_todo` (lib.rs:992) | `fs::metadata()` on intermediate dir |
| `ls <glob>` | nu_glob | `fill_todo` (lib.rs:999) | `fs::read_dir()` on intermediate dir |
| `open <file>` | nu_glob (via glob_from) | same as ls | same; then silently returns empty |
| `glob <pattern>` | wax/walkdir | `read_dir` on leaf dir | **works** — no intermediate walk |
| `path exists` | std | `fs::metadata` on full path | **works** — single call to leaf |

##### Verified behavior inside AppContainer (lot::spawn)

**Commands that FAIL (all due to nu_glob intermediate directory walk):**
- `ls` (no args) — "No matches found for Expand(`*`)"
- `ls <file-path>` — "No matches found for DoNotExpand(`...`)"
- `ls *.txt`, `ls **/*.txt` — "No matches found"
- `open <file>` (any variant) — silently returns `nothing`

**Commands that WORK (bypass nu_glob or use direct path access):**
- `ls <dir-path>`, `ls .` — uses `read_dir` on the granted directory directly
- `glob '*'`, `glob <exact-path>` — uses wax crate, not nu_glob
- `path exists`, `path type`, `path expand` — single `fs::metadata` on full path
- `save --force <file>` — direct file write, no glob resolution
- `pwd`, `cd` — no file lookup
- External commands via `^` — work (e.g., `rg`)

##### Fix options

**A. Fix reel's `cmd_setup`**: Pass the actual sandbox working directories (not just cwd) to `grant_appcontainer_prerequisites`, so all intermediates between root and the sandbox path get traverse ACEs.

**B. Make lot's `grant_acls` also grant ancestor traverse ACEs**: Would make every spawn self-contained but requires elevation at spawn time, breaking lot's two-tier design.

**C. Rewrite reel's nu custom commands**: Avoid `ls <file>` and `open <file>` entirely. Use `glob` + `ls <dir> | where` for metadata, and an alternative read mechanism (e.g., external command) for file content. Workaround — does not fix the ACL gap.

##### Nushell source references (tag 0.111.0)

- `crates/nu-glob/src/lib.rs:977-996` — `fill_todo`: component-by-component walk with `fs::metadata()`
- `crates/nu-engine/src/glob_from.rs:32-105` — `glob_from`: constructs absolute glob pattern, calls `nu_glob::glob_with()`
- `crates/nu-command/src/filesystem/ls.rs:441-468` — `ls` calls `glob_from`, checks `peek().is_none()` for empty iterator
- `crates/nu-command/src/filesystem/open.rs:110-126,258-259` — `open` calls `glob_from`, returns empty on zero matches
- `crates/nu-command/src/filesystem/glob.rs:197-251` — `glob` uses wax crate, walks via `walkdir::read_dir`

##### Diagnostic tests

In `reel/src/nu_session.rs`, prefixed with `diag_`. Run with `cargo test -p reel diag_ -- --nocapture`.

- `diag_intermediate_dir_metadata` — probes `path exists`/`path type` on every component from root to test file
- `diag_metadata_divergence` — shows `path exists` works but `ls <file>` fails for same path
- `diag_glob_from_path_flow` — shows `glob` works but `ls` fails for same pattern
- `diag_nu_mcp_without_lot` — proves all commands work without AppContainer
- `diag_raw_ls_in_sandbox` — shows `ls <file>` fails but `ls <dir>` works
- `diag_glob_vs_ls_appcontainer` — shows `glob '*'` works but `ls` (same pattern) fails
- `diag_bytestream_vs_string` — shows `open` returns `nothing`

**Category: Missing ancestor traverse ACEs on intermediate directories between volume root and sandbox policy paths.**

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

### 19. reel does not re-export lot's sandbox prerequisite APIs

`reel-cli/Cargo.toml` — `reel-cli` depends on `lot` directly and calls `lot::appcontainer_prerequisites_met` / `lot::grant_appcontainer_prerequisites`. Reel should re-export these via a `reel::sandbox` module so consumers don't depend on lot directly.

This is blocking for any library consumer of reel. Lot's spawn-time traverse ACE grants (rev `4e478de`) handle user-owned directories automatically, but system directories (e.g., `C:\Users`) require a one-time elevated setup via `grant_appcontainer_prerequisites`. Without reel re-exporting these APIs, library consumers cannot implement their own elevated setup command — they must add a direct lot dependency and track its version independently.

**Affected APIs that need re-export:**
- `grant_appcontainer_prerequisites(paths: &[&Path]) -> Result<()>`
- `grant_appcontainer_prerequisites_for_policy(policy: &SandboxPolicy) -> Result<()>`
- `appcontainer_prerequisites_met(paths: &[&Path]) -> bool`
- `appcontainer_prerequisites_met_for_policy(policy: &SandboxPolicy) -> bool`
- `is_elevated() -> bool`
- `SandboxError::PrerequisitesNotMet` variant (for matching spawn failures)

**Category: API completeness.**

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
