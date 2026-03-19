# Known Issues

Clusters ordered by impact/importance (highest first).

## 1. NuSession stderr and debuggability (#23)

Lost errors make debugging hard for all consumers.

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

## 2. Public API surface (#8, #31)

#31 is a semver hazard — flick internal type changes silently break reel's public API. Matters when external consumers exist.

### 31. `test_support` re-exports leak flick internal types through reel's public API

`reel/src/lib.rs` — When `testing` feature is enabled, types like `SingleShotProvider`, `DynProvider`, etc. are re-exported from flick. Changes to flick's test types break reel's API without any reel code changing. **Category: Semver hazard.**

### 8. `NuSession` re-exported but no external consumer

`reel/src/lib.rs` — `pub use nu_session::NuSession` is part of the public API but no consumer uses it directly. Consider removing after API stabilization. **Category: Placement.**

## 3. Tool execution coverage (#26)

### 26. Built-in file tools have fixed 120s timeout

`reel/src/tools.rs` — Only the NuShell tool respects model-provided timeout. File tools (Read, Write, Edit, Glob, Grep) use 120s. Slow Grep on large codebases cannot be extended. **Category: Feature gap.**

## 4. Nu glob robustness (#28)

Potential hang on pathological input with symlink cycles.

### 28. `reel glob` has no depth limit or symlink protection

`reel/build.rs` (REEL_CONFIG_NU) — `reel glob` runs `glob $pattern` with a 1000-result cap but no depth limit. A `**/*` pattern in a deep tree with symlink cycles could hang before the cap is reached. **Category: Robustness.**

## 5. Custom tool dispatch (#48)

No practical impact — unsupported scenario, already guarded by `build_request_config`.

### 48. Duplicate custom tool names: HashMap changes first-match to last-match semantics

`reel/src/agent.rs` — `dispatch_tool` previously used linear scan (first match wins). The `HashMap<String, usize>` built via `collect()` keeps the last entry for duplicate keys. No practical impact: duplicate custom tool names are not a supported scenario, and `build_request_config` rejects duplicate names against built-ins. **Category: Testing.**

## 6. Grant model refinements (#52)

### 52. `WRITE`/`NETWORK` → `TOOLS` implication not enforced at type level

`reel/src/tools.rs` — The implication (WRITE implies TOOLS, NETWORK implies TOOLS) is enforced only in `from_names`. Library consumers constructing `ToolGrant` directly via bitflags can create bare `WRITE` without `TOOLS`, which silently produces zero tool definitions. `tool_definitions` and `required_grant` defensively check `WRITE | TOOLS` together, duplicating the invariant. Consider a normalizing constructor or custom `BitOr` to enforce at the type level. **Category: Separation of concerns.**

## 7. CLI test coverage gaps (#68, #69)

### 68. `dry_run_output_includes_grant` tests serde round-trip, not production path

`reel-cli/src/main.rs` — The test constructs a `serde_json::json!` value inline and round-trips it through serde, rather than invoking the actual dry-run code path. A regression in `cmd_run`'s dry-run branch would not be caught. Needs test infrastructure to invoke `cmd_run` with a config file. **Category: Testing.**

### 69. No test guards against new `ToolGrant` variants omitted from `to_names`

`reel/src/tools.rs` — If a new `ToolGrant` variant is added but `to_names` is not updated, no test catches the omission. A test asserting `ToolGrant::all().to_names().len()` equals the expected variant count would guard against this. **Category: Testing.**

## 8. Test conditional skip conventions (#70)

### 70. `resolve_rg_binary_with_compile_time_tool_dir` silently skips

`reel/src/nu_session.rs` — When `NU_CACHE_DIR` is not set at compile time (common in local dev), the test prints "SKIP" to stderr and passes with zero assertions. Inflates test count without signal. Consider `#[ignore]` or a project-wide conditional-skip convention. **Category: Testing.**

## 9. Agent test provider consolidation (#71)

### 71. Agent test provider consolidation opportunity

`reel/src/agent.rs` — Five `DynProvider` test doubles (`RepeatingToolCallProvider`, `SlowProvider`, `FastThenSlowProvider`, plus inline `MultiShotProvider` usage) share nearly identical boilerplate. `FastThenSlowProvider` is a superset of `SlowProvider` (set first-call count to 0 for always-slow). Could be consolidated into fewer helpers as the test module grows. **Category: Simplification.**

## 10. Agent test improvements (#72, #73)

### 72. `ModelResponse` literal boilerplate in agent tests

`reel/src/agent.rs` — The pattern `ModelResponse { text: ..., thinking: Vec::new(), tool_calls: ..., usage: UsageResponse::default() }` is repeated many times. Helper functions like `fn tool_call_response(calls: Vec<ToolCallResponse>) -> ModelResponse` and `fn text_response(text: &str) -> ModelResponse` would reduce noise. **Category: Simplification.**

### 73. No test for exactly `MAX_TOOL_CALLS + 1` (201) boundary

`reel/src/agent.rs` — The boundary test checks exactly 200 (succeeds) and the exceeds test jumps to 250. No test for exactly 201 tool calls, which would more precisely verify the `>` vs `>=` check. **Category: Testing.**

## 11. Network test helper coverage (#74)

### 74. No regression tests for removed `looks_like_sandbox_denial` keywords

`reel/src/nu_session.rs` — When the keyword list was narrowed (issue #65), the old generic keywords (`"denied"`, `"permission"`, `"blocked"`, `"forbidden"`) were removed. The negative test (`sandbox_denial_rejects_non_denial_messages`) does not assert that these bare words are no longer matched. Adding explicit `assert!(!looks_like_sandbox_denial("denied"))` etc. would guard against accidental reversion. **Category: Testing.**
