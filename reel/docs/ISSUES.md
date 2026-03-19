# Known Issues

Clusters ordered by impact/importance (highest first).

## 1. NuSession stderr and debuggability (#23)

Lost errors make debugging hard for all consumers.

### 23. Nu stderr discarded

`reel/src/nu_session.rs` — `cmd.stderr(SandboxStdio::Null)` silently drops nu stderr. Errors outside JSON-RPC are lost. **Category: Debuggability.**

## 2. Tool definition repetition (#78, #79)

### 78. Timeout JSON property duplicated across 5 tool definitions

`reel/src/tools.rs` — The identical `"timeout": { "type": "integer", "description": "Timeout in seconds. Default: 120, max: 600." }` fragment is copy-pasted into 5 tool definition schemas. Could be extracted to a helper. **Category: Simplification.**

### 79. File tool timeout forwarding has no test coverage

`reel/src/tools.rs` — `parse_timeout()` is called for file tools in `execute_tool` but no test verifies that a model-provided timeout is forwarded to `nu_session.evaluate()`. The NuShell tool has the same gap. **Category: Testing.**
