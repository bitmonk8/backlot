# Spec: Unified Tool Layer via Nu Custom Commands

**Status**: Draft — not yet decided.

## Summary

Move epic's file tools (`read_file`, `write_file`, `edit_file`, `glob`, `grep`) out of epic's Rust process and into the nu MCP session as nu custom commands. All agent tool calls would route through the sandboxed nu process, eliminating the dual-enforcement model (safe_path + lot) in favor of lot-only sandboxing.

## Motivation

Epic currently enforces filesystem boundaries two ways:

1. **`safe_path()` in tools.rs** — path canonicalization and symlink guards, applied to file tools running in epic's own process.
2. **lot sandbox** — OS-level process isolation on the nu process.

This creates three TOCTOU race conditions (see git history, formerly AUDIT.md) that are not practically exploitable but exist because epic's process is unsandboxed. Moving all file operations into the sandboxed nu process eliminates the race class by construction — lot enforces boundaries at the syscall level, before any file handle is opened.

## Current Tool Grant Model

| Phase | Grant Flags | Tools Available |
|-------|-------------|-----------------|
| Analyze | `READ` | read_file, glob, grep |
| Execute | `READ \| WRITE \| NU` | read_file, glob, grep, write_file, edit_file, nu |
| Decompose | `READ \| NU` | read_file, glob, grep, nu |

Analyze-phase sessions have no nu access today.

## Proposed Change

All file tools become nu custom commands. Every agent session gets a nu MCP session. Lot's per-phase sandbox policy replaces ToolGrant as the access control mechanism.

### Phase → Lot Policy Mapping

| Phase | Lot Policy | Effect |
|-------|-----------|--------|
| Analyze | read_path(project_root) | File tools work read-only. Nu commands cannot write. |
| Execute | write_path(project_root) | Full access. |
| Decompose | read_path(project_root) | Read + nu commands, no writes. |

### What Changes

- `tool_read_file`, `tool_write_file`, `tool_edit_file`, `tool_glob`, `tool_grep` removed from tools.rs.
- Equivalent nu custom commands registered at session startup.
- `safe_path()` and `verify_ancestors_within_root()` removed (lot enforces boundaries).
- `ToolGrant::READ` and `ToolGrant::WRITE` flags become unused — lot policy is the sole gatekeeper.
- `ToolGrant` may collapse to just `NU` + `TASK` + `WEB`, or be removed entirely if all tools route through nu.
- `execute_tool()` dispatch simplifies: all tool calls go to `nu_session.evaluate()` (or a tool-call variant).

### Nu Custom Commands

The custom commands would preserve the current tool semantics (size limits, output caps, etc.) but implemented in nu:

| Command | Equivalent | Notes |
|---------|-----------|-------|
| `epic read <path>` | `tool_read_file` | 256 KiB cap, returns content |
| `epic write <path> <content>` | `tool_write_file` | 1 MiB cap |
| `epic edit <path> <old> <new>` | `tool_edit_file` | Exact substring replacement |
| `epic glob <pattern>` | `tool_glob` | 1000 result cap |
| `epic grep <pattern> <path?>` | `tool_grep` | 64 KiB output cap, 10 MiB file cap |

Exact command names and signatures TBD. Could also be registered as MCP tools rather than nu custom commands.

## Advantages

1. **Eliminates TOCTOU races by construction.** Lot enforces path boundaries at the OS level before any file handle opens. No validation-then-use gap.
2. **Single sandboxing mechanism.** Removes the `safe_path` + `ToolGrant` + lot layering. One mechanism to reason about.
3. **Less Rust code to maintain.** ~450 lines of file tool implementations in tools.rs replaced by simpler nu commands.
4. **Consistent security model.** Every agent action — file reads, writes, shell commands — goes through the same sandbox.

## Disadvantages and Open Questions

### All sessions now require a nu process

Analyze-phase sessions (assessment, verification, checkpoints) currently use no nu session. This change would spawn a nu MCP process for every agent call, increasing resource usage. Assessment calls are frequent and cheap (single Haiku call, no tools used most of the time).

**Mitigation options:**
- Lazy spawn: only start nu when the first tool call arrives (current behavior for nu-granting phases).
- Accept the cost: nu startup is fast, and sessions are short-lived.
- Keep `read_file`/`glob`/`grep` in-process for Analyze phase only — but this reintroduces the dual model.

### Write access exposure

Today, Analyze-phase sessions cannot call `write_file` because `ToolGrant::WRITE` is not granted. With unified nu, the question becomes: does lot's read-only policy on the project root actually prevent writes?

**Answer: yes.** Lot's `read_path()` uses OS-level enforcement (namespaces + seccomp on Linux, Seatbelt on macOS, AppContainer on Windows) to make the path read-only to the process. The nu process literally cannot open files for writing. This is stronger than ToolGrant, which is advisory (enforced in epic's Rust code, bypassable if there's a bug).

However, this needs verification per platform. The lot sandbox must be tested to confirm:
- `read_path` truly prevents writes, renames, symlink creation, and hardlink creation.
- `write_path` does not grant access outside the specified path.
- Temp dir access does not provide a pivot to project root.

### Performance

File tool calls currently use direct `tokio::fs` operations. With nu, each call adds:
- JSON-RPC serialization/deserialization
- IPC round-trip over stdin/stdout
- Nu command parsing and execution

Likely negligible for individual calls. Could matter for grep over large trees or rapid glob calls. Needs measurement.

### Error fidelity

Current Rust implementations return structured errors (path not found, permission denied, size limit exceeded). Nu custom commands would need to match this error reporting for agents to recover correctly.

### MCP tool registration

An alternative to nu custom commands: register the file tools as additional MCP tools on the nu server. This would preserve the current tool-call interface exactly (same JSON schema, same dispatch) but route execution through the sandboxed process. Requires understanding nu's MCP tool registration API.

## Non-Goals

- Changing the tool semantics (size limits, output formats).
- Adding new tools.
- Changing the agent prompt format.
