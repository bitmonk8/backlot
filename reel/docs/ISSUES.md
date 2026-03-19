# Known Issues

Clusters ordered by impact/importance (highest first).

## 1. NuSession stderr and debuggability (#23)

Lost errors make debugging hard for all consumers.

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

## 2. Timeout parsing edge cases (#80, #81)

### 80. parse_timeout allows zero timeout

`reel/src/tools.rs` — `parse_timeout()` returns `0` for `{"timeout": 0}`, which would cause immediate expiration. Design decision needed: add a lower clamp (e.g., 1s) or document zero as valid. **Category: Correctness.**

### 81. Timeout test coverage gaps

`reel/src/tools.rs` — Several minor gaps: `test_parse_timeout_non_integer_falls_back` only tests string, not negative numbers. `test_with_timeout_adds_property` doesn't verify description field. `test_all_tool_definitions_have_timeout` checks key existence but not schema shape. `test_with_timeout_no_properties_key` doesn't cover non-object `properties` value. **Category: Testing.**
