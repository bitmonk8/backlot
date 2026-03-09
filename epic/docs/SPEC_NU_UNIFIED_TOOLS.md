# Spec: Unified Tool Layer via Nu Custom Commands

**Status**: Draft — decisions recorded, approaching implementation-ready.

## Summary

Move epic's file tools (`read_file`, `write_file`, `edit_file`, `glob`, `grep`) out of epic's Rust process and into the nu MCP session as nu custom commands. All agent tool calls route through the sandboxed nu process, eliminating the dual-enforcement model (safe_path + lot) in favor of lot-only sandboxing.

## Motivation

Epic currently enforces filesystem boundaries two ways:

1. **`safe_path()` in tools.rs** — path canonicalization and symlink guards, applied to file tools running in epic's own process.
2. **lot sandbox** — OS-level process isolation on the nu process.

This creates three TOCTOU race conditions (see git history, formerly AUDIT.md) that are not practically exploitable but exist because epic's process is unsandboxed. Moving all file operations into the sandboxed nu process eliminates the race class by construction — lot enforces boundaries at the syscall level, before any file handle is opened.

## Decisions

### D1: Lazy spawn (decided)

Nu processes spawn lazily on first tool call. Three agent call types never receive tools (assessment, checkpoint, assess-recovery) and never spawn nu. All other agent types (verify, execute, decompose, fix, recovery-design) receive tools and their prompts explicitly instruct tool use — meaning they will call tools in virtually every session. Lazy spawn adds at most one tool-call of latency for those sessions.

### D2: Custom commands via evaluate, not MCP tool registration (decided)

Nu 0.111.0 does not support registering custom MCP tools. Nu's built-in MCP server exposes only `evaluate`, `find_command`, and `list_command`. Custom commands are defined via `def` in nu script and invoked through the `evaluate` tool. This is simpler than MCP registration and gives full access to nu's scripting capabilities.

### D3: Lot sandbox is the sole access control mechanism (decided)

`safe_path()`, `verify_ancestors_within_root()`, and `ToolGrant::READ`/`WRITE` flags are removed. Lot's per-phase sandbox policy is the sole gatekeeper. `ToolGrant` collapses or is removed entirely (all tool calls route through nu `evaluate`).

### D4: Platform sandbox verification (confirmed)

Lot provides OS-level read/write/execute path controls on all three platforms:

| Capability | Linux | macOS | Windows |
|---|---|---|---|
| Read-only enforcement | Mount NS (`MS_RDONLY` remount) | Seatbelt SBPL (`file-read*` only) | AppContainer ACLs (`FILE_GENERIC_READ`) |
| Read-write enforcement | Mount NS (no `MS_RDONLY`) | Seatbelt (`file-read*` + `file-write*`) | ACLs (`FILE_GENERIC_READ \| WRITE`) |
| Executable control | Mount flags (`MS_NOEXEC`) | Seatbelt (`process-exec`, `file-map-executable`) | ACLs (`FILE_GENERIC_EXECUTE`) |
| Path hiding | Full (only mounted paths exist) | Default-deny (access denied) | ACL-based (access denied) |
| Always available? | Requires unprivileged user NS | Always | Always |

**Path hiding difference**: On Linux, unmounted paths literally don't exist in the mount namespace. On macOS/Windows, paths exist but access is denied. For epic's use case (agents work within project root) this is equivalent — agents cannot read or write outside the sandbox boundary.

**Linux caveat**: Unprivileged user namespaces may be disabled by kernel config or AppArmor. This is an existing constraint (epic already depends on lot for nu), not a new one.

---

## Current Tool Grant Model (before)

| Phase | Grant Flags | Tools Available |
|-------|-------------|-----------------|
| Analyze | `READ` | read_file, glob, grep |
| Execute | `READ \| WRITE \| NU` | read_file, glob, grep, write_file, edit_file, nu |
| Decompose | `READ \| NU` | read_file, glob, grep, nu |

Assessment, checkpoint, and assess-recovery receive zero tools and use `run_structured()` (no tool loop).

## Proposed Model (after)

### Phase → Lot Policy Mapping

| Phase | Lot Policy | Tools Available | Effect |
|-------|-----------|-----------------|--------|
| Analyze (verify, file-review) | `read_path(project_root)` | All commands via `evaluate` | Read-only. OS prevents writes. |
| Execute (leaf, fix-leaf) | `write_path(project_root)` | All commands via `evaluate` | Full read-write access. |
| Decompose (design, recovery-design) | `read_path(project_root)` | All commands via `evaluate` | Read + nu commands, OS prevents writes. |
| Assess / Checkpoint | N/A | None | No nu process spawned. No tools. |

### What Changes

- `tool_read_file`, `tool_write_file`, `tool_edit_file`, `tool_glob`, `tool_grep` removed from `tools.rs`.
- `safe_path()` and `verify_ancestors_within_root()` removed.
- `ToolGrant::READ` and `ToolGrant::WRITE` flags removed. `ToolGrant` type removed or reduced to a marker for "has nu access".
- `execute_tool()` dispatch simplified: all tool calls go to `nu_session.evaluate()`.
- `AgentMethod` enum simplified: `HasTools` (spawns nu) vs `NoTools` (structured output, no tool loop).
- Nu custom commands defined at session startup via `--commands` flag or initial `evaluate` call.

---

## Nu Custom Command Definitions

### Loading mechanism

At session startup, before any agent tool calls, epic sends an `evaluate` call containing all custom command definitions as nu script. This is a single MCP `tools/call` request with the `def` blocks concatenated. The definitions persist in the nu session for subsequent `evaluate` calls.

Alternative: pass definitions via `nu --commands "..." --mcp`. Needs testing — `--commands` may conflict with `--mcp` mode. If it works, it avoids the extra evaluate round-trip.

### Command definitions

Each command preserves the semantics of the current Rust implementation. Error reporting uses nu's structured error mechanism (`error make`).

```nu
# read_file — read file contents, 256 KiB cap
def "epic read" [path: string] {
    let full = ($path | path expand)
    let size = (ls $full | get size | first)
    if $size > 262144 {
        error make {
            msg: $"File too large: ($size) bytes, max 262144"
            label: { text: "size limit", span: (metadata $path).span }
        }
    }
    open $full --raw | decode utf-8
}

# write_file — write content to file, 1 MiB cap
def "epic write" [path: string, content: string] {
    let size = ($content | str length)
    if $size > 1048576 {
        error make {
            msg: $"Content too large: ($size) bytes, max 1048576"
        }
    }
    let full = ($path | path expand)
    # Ensure parent directory exists
    let parent = ($full | path dirname)
    mkdir $parent
    $content | save --force $full
}

# edit_file — exact substring replacement
def "epic edit" [path: string, old: string, new: string] {
    let full = ($path | path expand)
    let content = (open $full --raw | decode utf-8)
    let count = ($content | str index-of $old | length)  # TBD: nu API for count
    if $count == 0 {
        error make { msg: "Substring not found" }
    }
    if $count > 1 {
        error make { msg: $"Substring found ($count) times, must be unique" }
    }
    let result = ($content | str replace $old $new)
    $result | save --force $full
}

# glob — find files by pattern, 1000 result cap
def "epic glob" [pattern: string] {
    glob $pattern | first 1000 | to text
}

# grep — search file contents, 64 KiB output cap, 10 MiB file size cap
def "epic grep" [
    pattern: string
    path?: string
] {
    # TBD: implementation using nu's built-in grep/find capabilities
    # Must respect: 64 KiB output cap, 10 MiB per-file cap, regex support
}
```

**These are illustrative, not final.** The exact nu API calls, error handling patterns, and output formatting need validation against nu 0.111.0. Key areas requiring prototype testing:

1. **`epic edit` substring counting** — nu's string API for counting non-overlapping occurrences.
2. **`epic grep` implementation** — nu has no built-in grep equivalent with regex. Options: `rg` via shell-out (requires rg in sandbox), manual `open` + `lines` + `where` pipeline, or `str contains`/`str index-of` per line.
3. **`epic glob` path relativity** — confirm glob patterns resolve relative to cwd (project root) within the sandbox.
4. **Binary file handling** — `open --raw` returns bytes; `decode utf-8` may fail on binary files. Need error handling.
5. **Output format** — current Rust tools return structured JSON. Nu `evaluate` returns the output as a string. Agents currently receive tool results as text, so this should be compatible, but needs verification.

### Tool descriptions for agent prompts

Agents see tool descriptions in their system prompt. With unified nu, agents see a single tool:

```
evaluate: Execute a NuShell command or pipeline.

Available commands:
  epic read <path>       — Read file contents (max 256 KiB)
  epic write <path> <content> — Write file (max 1 MiB)  [execute phase only]
  epic edit <path> <old> <new> — Replace exact substring  [execute phase only]
  epic glob <pattern>    — Find files by glob pattern (max 1000 results)
  epic grep <pattern> [path] — Search file contents by regex (max 64 KiB output)

  You can also run arbitrary NuShell commands and pipelines.
```

Write commands (`epic write`, `epic edit`) are listed in all prompts but enforced by the sandbox — if an analyze-phase agent tries to write, the OS blocks it and nu returns a permission error. The agent prompt can optionally omit write commands for read-only phases to reduce confusion, but security does not depend on it.

**Decision (D5)**: Omit write commands from read-only phase prompts. Two prompt variants: read-only (analyze/decompose) lists `epic read`, `epic glob`, `epic grep` only; read-write (execute) lists all five commands. Security does not depend on this (sandbox enforces regardless), but it avoids confusing agents with tools that will fail.

---

## Implementation Plan

### Phase 1: Nu custom command prototyping

1. Write and test nu custom command definitions against nu 0.111.0.
2. Validate: `--commands` + `--mcp` compatibility.
3. Validate: error reporting via `error make` surfaces correctly through MCP `evaluate` responses.
4. Validate: output format compatibility (string output vs structured).
5. Validate: `epic glob` and `epic grep` behavior within sandbox boundaries.

### Phase 2: Session startup injection

1. Modify `NuSession::initialize()` to send custom command definitions after MCP handshake.
2. Test: commands persist across subsequent `evaluate` calls in the same session.
3. Test: command definitions don't interfere with raw `evaluate` calls (agents can still run arbitrary nu).

### Phase 3: Tool layer migration

1. Remove `tool_read_file`, `tool_write_file`, `tool_edit_file`, `tool_glob`, `tool_grep` from `tools.rs`.
2. Remove `safe_path()`, `verify_ancestors_within_root()`.
3. Remove `ToolGrant::READ`, `ToolGrant::WRITE`. Simplify or remove `ToolGrant`.
4. Simplify `execute_tool()` — all calls route to `nu_session.evaluate()`.
5. Update `AgentMethod` to `HasTools` / `NoTools`.
6. Update tool descriptions in prompt assembly (`prompts.rs`).

### Phase 4: Sandbox policy consolidation

1. Verify lot `read_path` prevents writes on all platforms (automated tests).
2. Verify temp dir access cannot pivot to project root.
3. Remove any remaining `safe_path` references.
4. Update DESIGN.md and README.md to reflect unified model.

---

## Risks

### grep implementation complexity

Current Rust `tool_grep` uses the `regex` and `walkdir` crates for recursive regex search with file-size filtering. Nu has no direct equivalent. Options:

| Approach | Pros | Cons |
|---|---|---|
| Shell out to `rg` (ripgrep) | Feature-complete, fast | Requires rg binary in sandbox; extra dependency |
| Nu `open` + `lines` + `find` pipeline | No extra dependency | Slow on large trees, limited regex |
| Bundle a grep script in nu | Self-contained | Complex, maintenance burden |

**Recommendation**: Ship `rg` alongside `nu` in the build (same download-and-cache pattern in `build.rs`). `rg` is a single static binary, widely available, and already the industry standard for code search. The `epic grep` command becomes a thin wrapper around `rg --json` with output capping.

### Performance on large repositories

File tool calls gain IPC overhead (~1ms per call). For typical agent sessions (10-50 tool calls), this adds <50ms total. For pathological grep-over-large-tree cases, the bottleneck is I/O, not IPC.

### Error message fidelity

Agents rely on error messages to recover (e.g., "file not found" vs "permission denied" vs "size limit exceeded"). Nu's `error make` produces structured errors that surface through MCP `evaluate` responses. The error text must be clear enough for the agent to act on. Test this explicitly during Phase 1.

### Sandbox policy for temp dirs

Lot grants writable temp dir access on all platforms. This is necessary (nu needs temp space for internal operations). Temp dirs are outside the project root, so an agent writing to temp cannot affect project files. However, an agent could use temp as scratch space to work around read-only project root restrictions (read file → copy to temp → modify in temp). This is not a security concern (the agent can't write the result back to the project root), but it's worth noting.

## Non-Goals

- Changing the tool semantics (size limits, output formats).
- Adding new tools beyond the existing six.
- Changing the agent prompt format (beyond tool descriptions).
- Parallel nu sessions within a single agent call.
