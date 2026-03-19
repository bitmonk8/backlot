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

## 5. Test coverage gaps (#75, #76, #77)

### 75. Tool call cap boundary test only crosses at round boundaries

`reel/src/agent.rs` — `run_with_tools_exactly_max_tool_calls_plus_one_fails` uses 67 calls/round, so the cap is crossed at a round boundary (134→201). A test where cumulative count crosses 200 mid-round (e.g. 150 calls in round 1, then 51 in round 2) would better verify the `>` check. **Category: Testing.**

### 76. CLI dry-run test does not assert specific tool names

`reel-cli/src/main.rs` — `dry_run_output_includes_grant` asserts `tools` is non-empty but does not check which tools appear. If the grant-to-tool mapping changes, the test would still pass as long as any tool is present. **Category: Testing.**

### 77. `duplicate_custom_tool_names_uses_last_match` tests HashMap semantics, not production code

`reel/src/agent.rs` — The test constructs a HashMap in isolation rather than exercising the production `custom_tool_index` construction path. A change from HashMap to another collection type would not be caught. **Category: Testing.**
