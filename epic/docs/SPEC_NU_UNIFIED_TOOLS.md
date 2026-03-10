# Spec: Unified Tool Layer via Nu Custom Commands

**Status**: Draft — decisions recorded, approaching implementation-ready.

## Summary

Move epic's file tools (`read_file`, `write_file`, `edit_file`, `glob`, `grep`) out of epic's Rust process and into the nu MCP session as nu custom commands. All file I/O routes through the sandboxed nu process, eliminating the dual-enforcement model (safe_path + lot) in favor of lot-only sandboxing.

Agent-facing tool schemas are modeled on Claude Code's tool interface. Claude models are trained extensively on these schemas, so alignment reduces friction and improves tool-use accuracy. Epic translates JSON tool calls into nu commands internally — agents never write nu syntax for file operations.

## Motivation

### TOCTOU elimination

Epic currently enforces filesystem boundaries two ways:

1. **`safe_path()` in tools.rs** — path canonicalization and symlink guards, applied to file tools running in epic's own process.
2. **lot sandbox** — OS-level process isolation on the nu process.

This creates three TOCTOU race conditions (see git history, formerly AUDIT.md) that are not practically exploitable but exist because epic's process is unsandboxed. Moving all file operations into the sandboxed nu process eliminates the race class by construction — lot enforces boundaries at the syscall level, before any file handle is opened.

### Claude Code tool alignment

Epic recommends Anthropic's Claude models (any model available through Flick can be used). Claude Code (Anthropic's CLI agent) exposes a specific set of tool schemas that Claude models are trained on: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `Bash`. Since Claude is the recommended model family, aligning epic's tool schemas with Claude Code's interface reduces tool-use errors and improves parameter utilization for the default case, while remaining usable by other models. Epic's shell tool is named `NuShell` (not `Bash`) to steer models toward NuShell syntax, but its parameter schema mirrors Claude Code's `Bash` tool.

## Decisions

### D1: Eager spawn for tool-granted sessions (decided)

Nu processes spawn eagerly at session creation for any agent call that receives tool grants. Three agent call types never receive tools (assessment, checkpoint, assess-recovery) and skip spawning entirely. All other agent types (verify, execute, decompose, fix, recovery-design) spawn nu immediately — these sessions use tools in virtually every invocation, so lazy spawn only added first-call latency without meaningful savings.

### D2: Separate tool schemas, routed through nu internally (decided)

**Revised from earlier draft.** Agents see separate tool definitions with JSON parameter schemas (Read, Write, Edit, Glob, Grep, NuShell) — not a single `evaluate` tool. Epic's tool executor translates each JSON tool call into a nu command and dispatches it through `nu_session.evaluate()`. This is invisible to agents.

Rationale: Claude models are trained on Claude Code's tool interface where each tool is a distinct callable with typed parameters. Collapsing everything into a single `evaluate` tool would force agents to compose nu syntax for basic file operations — an unfamiliar interface that increases error rates. Keeping separate tools with Claude Code-aligned schemas preserves the training advantage.

The NuShell tool maps directly to nu's `evaluate` for arbitrary commands. Nu custom commands are loaded at session startup to implement the file tools.

Nu 0.111.0 does not support registering custom MCP tools (only exposes `evaluate`, `find_command`, `list_command`). The separation is maintained in epic's Rust layer, not in nu's MCP server.

### D3: Lot sandbox is the sole access control mechanism (decided)

`safe_path()`, `verify_ancestors_within_root()`, and `ToolGrant::READ`/`WRITE` flags are removed. Lot's per-phase sandbox policy is the sole gatekeeper. `ToolGrant` collapses to a phase marker controlling which tool definitions are offered to the agent.

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

### D5: Phase-based tool filtering (decided)

Two tool sets offered to agents based on phase:

- **Read-only phases** (analyze, decompose, verify): Read, Glob, Grep, NuShell
- **Read-write phases** (execute, fix): Read, Write, Edit, Glob, Grep, NuShell

Security does not depend on this filtering — the lot sandbox enforces access regardless. Filtering prevents agents from wasting tokens on tool calls that will fail.

### D6: Claude Code tool schema alignment (decided)

Tool names, parameter names, and parameter semantics are modeled on Claude Code's tool interface. Specific alignment choices:

| Aspect | Claude Code | Epic | Notes |
|---|---|---|---|
| Tool names | Read, Write, Edit, Glob, Grep, Bash | Read, Write, Edit, Glob, Grep, NuShell | Shell tool renamed to steer toward NuShell syntax |
| Read params | `file_path`, `offset`, `limit` | Same | Line-based pagination added |
| Write params | `file_path`, `content` | Same | Renamed from `path` |
| Edit params | `file_path`, `old_string`, `new_string`, `replace_all` | Same | `replace_all` added |
| Glob params | `pattern`, `path` | Same | Already aligned |
| Grep params | Full ripgrep interface | Subset | See Grep section below |
| Bash params | `command`, `description`, `timeout` | Same schema, named `NuShell` | Param schema from CC Bash, name steers toward nu syntax |

**Deliberately omitted from Claude Code**: `run_in_background` (epic sessions are single-threaded), `offset` on Grep (unnecessary with `head_limit`), `NotebookEdit` (not relevant), all IDE/agent/web tools (epic has its own equivalents).

### D7: File tool forwarders are configurable (decided)

The file tools (Read, Write, Edit, Glob, Grep) are convenience forwarders — they translate JSON tool calls into nu custom commands. They are not strictly necessary: the NuShell tool can invoke the same nu custom commands directly, and nu's `list_command`/`find_command` MCP tools make the custom commands discoverable by agents.

Since only Claude models are expected to benefit from the familiar tool schemas, the forwarders are configurable:

```toml
[agent]
# Expose file tools (Read, Write, Edit, Glob, Grep) as separate tool definitions
# that forward to nu custom commands. When false, only the NuShell tool is offered
# and agents use nu commands directly. Default: true.
file_tool_forwarders = true
```

When `file_tool_forwarders = false`:
- Only the NuShell tool is offered to agents (plus phase filtering on the system prompt stating which operations are permitted).
- Nu custom commands (`epic_read`, `epic_write`, etc.) are still loaded at session startup — agents can invoke them via the NuShell tool.
- The nu MCP server's `find_command`/`list_command` tools make the custom commands discoverable.
- The translation layer in `execute_tool()` is bypassed entirely.

When `file_tool_forwarders = true` (default):
- Read, Write, Edit, Glob, Grep are offered as separate tools alongside NuShell.
- `execute_tool()` translates their JSON params to nu commands.
- Agents can still use the NuShell tool directly for anything the forwarders cover.

This enables A/B comparison of tool-use accuracy with and without forwarders.

---

## Current Tool Grant Model (before)

| Phase | Grant Flags | Tools Available |
|-------|-------------|-----------------|
| Analyze | `READ` | read_file, glob, grep |
| Execute | `READ \| WRITE \| NU` | read_file, glob, grep, write_file, edit_file, nu |
| Decompose | `READ \| NU` | read_file, glob, grep, nu |

Assessment, checkpoint, and assess-recovery receive zero tools and use `run_structured()` (no tool loop).

## Proposed Model (after)

### Phase → Lot Policy → Tool Set

With `file_tool_forwarders = true` (default):

| Phase | Lot Policy | Tools Offered | Effect |
|-------|-----------|---------------|--------|
| Analyze (verify, file-review) | `read_path(project_root)` | Read, Glob, Grep, NuShell | Read-only. OS prevents writes. |
| Execute (leaf, fix-leaf) | `write_path(project_root)` | Read, Write, Edit, Glob, Grep, NuShell | Full read-write access. |
| Decompose (design, recovery-design) | `read_path(project_root)` | Read, Glob, Grep, NuShell | Read + shell commands, OS prevents writes. |
| Assess / Checkpoint | N/A | None | No nu process spawned. No tools. |

With `file_tool_forwarders = false`:

| Phase | Lot Policy | Tools Offered | Effect |
|-------|-----------|---------------|--------|
| Analyze (verify, file-review) | `read_path(project_root)` | NuShell | Read-only. OS prevents writes. |
| Execute (leaf, fix-leaf) | `write_path(project_root)` | NuShell | Full read-write access. |
| Decompose (design, recovery-design) | `read_path(project_root)` | NuShell | Read + shell commands, OS prevents writes. |
| Assess / Checkpoint | N/A | None | No nu process spawned. No tools. |

In both modes, nu custom commands (`epic_read`, `epic_write`, etc.) are loaded at session startup and available through the NuShell tool.

### What Changes

- Rust tool implementations (`tool_read_file`, `tool_write_file`, `tool_edit_file`, `tool_glob`, `tool_grep`) removed from `tools.rs`.
- `safe_path()` and `verify_ancestors_within_root()` removed.
- `ToolGrant::READ` and `ToolGrant::WRITE` flags removed. `ToolGrant` reduced to phase marker (HasTools / NoTools).
- `execute_tool()` conditionally translates JSON tool params → nu command string → `nu_session.evaluate()` (when forwarders enabled).
- Tool names changed: `read_file` → `Read`, `write_file` → `Write`, `edit_file` → `Edit`, `nu` → `NuShell`.
- Tool schemas enriched to match Claude Code (see below).
- Nu custom commands loaded at session startup regardless of forwarder setting.
- New config field: `[agent] file_tool_forwarders` (bool, default true).

---

## Tool Schemas

### Read

Read file contents with optional line-based pagination.

```json
{
  "name": "Read",
  "description": "Read the contents of a file. Returns lines with line numbers. For large files, use offset and limit to read specific sections.",
  "parameters": {
    "type": "object",
    "properties": {
      "file_path": {
        "type": "string",
        "description": "Absolute or project-relative file path"
      },
      "offset": {
        "type": "integer",
        "description": "Line number to start reading from (1-based). Omit to start from the beginning."
      },
      "limit": {
        "type": "integer",
        "description": "Maximum number of lines to return. Omit to read up to the default cap."
      }
    },
    "required": ["file_path"]
  }
}
```

**Differences from Claude Code**: `file_path` accepts project-relative paths (Claude Code requires absolute). Default `limit` is governed by the 256 KiB output cap rather than a fixed line count.

**Translation**: `epic_read $file_path --offset $offset --limit $limit`

### Write

Create or overwrite a file.

```json
{
  "name": "Write",
  "description": "Write content to a file, creating parent directories if necessary. Overwrites existing files.",
  "parameters": {
    "type": "object",
    "properties": {
      "file_path": {
        "type": "string",
        "description": "File path to write to"
      },
      "content": {
        "type": "string",
        "description": "Content to write"
      }
    },
    "required": ["file_path", "content"]
  }
}
```

**Differences from Claude Code**: None meaningful. Size cap (1 MiB) enforced by the nu command.

**Translation**: `epic_write $file_path $content`

### Edit

Replace exact string in a file.

```json
{
  "name": "Edit",
  "description": "Replace an exact string match in a file. By default, old_string must appear exactly once (prevents ambiguous edits). Set replace_all to replace every occurrence.",
  "parameters": {
    "type": "object",
    "properties": {
      "file_path": {
        "type": "string",
        "description": "File path to edit"
      },
      "old_string": {
        "type": "string",
        "description": "Exact text to find and replace"
      },
      "new_string": {
        "type": "string",
        "description": "Replacement text"
      },
      "replace_all": {
        "type": "boolean",
        "description": "Replace all occurrences instead of requiring uniqueness. Default: false."
      }
    },
    "required": ["file_path", "old_string", "new_string"]
  }
}
```

**Differences from Claude Code**: None. Schema matches exactly.

**Translation**: `epic_edit $file_path $old_string $new_string --replace-all=$replace_all`

### Glob

Find files by glob pattern.

```json
{
  "name": "Glob",
  "description": "Find files matching a glob pattern. Returns matching file paths sorted by modification time.",
  "parameters": {
    "type": "object",
    "properties": {
      "pattern": {
        "type": "string",
        "description": "Glob pattern (e.g. **/*.rs, src/**/*.ts)"
      },
      "path": {
        "type": "string",
        "description": "Directory to search in. Defaults to project root."
      }
    },
    "required": ["pattern"]
  }
}
```

**Differences from Claude Code**: None. Schema matches exactly.

**Translation**: `epic_glob $pattern --path $path`

### Grep

Search file contents by regex. Powered by ripgrep.

```json
{
  "name": "Grep",
  "description": "Search file contents for a regex pattern. Powered by ripgrep.",
  "parameters": {
    "type": "object",
    "properties": {
      "pattern": {
        "type": "string",
        "description": "Regex pattern to search for"
      },
      "path": {
        "type": "string",
        "description": "File or directory to search in. Defaults to project root."
      },
      "output_mode": {
        "type": "string",
        "enum": ["content", "files_with_matches", "count"],
        "description": "Output mode. 'content' shows matching lines, 'files_with_matches' shows only file paths (default), 'count' shows match counts."
      },
      "glob": {
        "type": "string",
        "description": "Glob pattern to filter files (e.g. *.js, **/*.tsx)"
      },
      "include_type": {
        "type": "string",
        "description": "File type filter (e.g. js, py, rust, go). Maps to rg --type."
      },
      "case_insensitive": {
        "type": "boolean",
        "description": "Case insensitive search. Default: false."
      },
      "line_numbers": {
        "type": "boolean",
        "description": "Show line numbers in output. Default: true. Only applies to 'content' output mode."
      },
      "context_after": {
        "type": "integer",
        "description": "Number of lines to show after each match. Only applies to 'content' output mode."
      },
      "context_before": {
        "type": "integer",
        "description": "Number of lines to show before each match. Only applies to 'content' output mode."
      },
      "context": {
        "type": "integer",
        "description": "Number of lines to show before and after each match. Only applies to 'content' output mode."
      },
      "multiline": {
        "type": "boolean",
        "description": "Enable multiline matching (pattern can span lines). Default: false."
      },
      "head_limit": {
        "type": "integer",
        "description": "Limit output to first N lines/entries."
      }
    },
    "required": ["pattern"]
  }
}
```

**Differences from Claude Code**: Parameter names use `snake_case` instead of `-` prefixed flags (`case_insensitive` instead of `-i`, `context_after` instead of `-A`, etc.). This is because epic's tools are JSON schemas, not CLI flags — descriptive names are clearer. Claude models will recognize the semantics regardless.

Claude Code's `type` parameter is renamed to `include_type` to avoid collision with JSON Schema's `type` keyword.

**Translation**: `epic_grep $pattern --path $path --output-mode $output_mode --glob $glob --type $include_type --case-insensitive=$case_insensitive --line-numbers=$line_numbers --context-after=$context_after --context-before=$context_before --context=$context --multiline=$multiline --head-limit=$head_limit`

### NuShell

Execute a NuShell command. Replaces the former `nu` tool. Parameter schema mirrors Claude Code's `Bash` tool, but the name and description steer models toward NuShell syntax.

```json
{
  "name": "NuShell",
  "description": "Execute a NuShell command or pipeline and return its output. This tool is the NuShell equivalent of Claude Code's Bash tool — same role, but uses NuShell syntax instead of POSIX sh. Session state (variables, env, cwd) persists across calls within the same task.",
  "parameters": {
    "type": "object",
    "properties": {
      "command": {
        "type": "string",
        "description": "The NuShell command to execute"
      },
      "description": {
        "type": "string",
        "description": "Brief description of what this command does"
      },
      "timeout": {
        "type": "integer",
        "description": "Timeout in seconds. Default: 120, max: 600."
      }
    },
    "required": ["command"]
  }
}
```

**Differences from Claude Code**: Named `NuShell` instead of `Bash` to steer models toward NuShell syntax. Executes NuShell syntax, not POSIX sh. No `run_in_background` (epic sessions are single-threaded). The `description` parameter is accepted for logging/TUI display but does not affect execution.

**Translation**: Direct pass-through to `nu_session.evaluate(command)`.

---

## Tool Executor: JSON → Nu Translation

When `file_tool_forwarders = true`, epic's `execute_tool()` receives a JSON tool call from Flick, translates parameters into a nu command string, and dispatches through `nu_session.evaluate()`. When forwarders are disabled, only the NuShell tool is offered — agents invoke nu custom commands directly.

### Translation layer (Rust)

```rust
fn translate_tool_call(name: &str, params: &serde_json::Value) -> Result<String> {
    match name {
        "Read" => {
            let path = params["file_path"].as_str().required()?;
            let mut cmd = format!("epic_read {}", quote_nu(path));
            if let Some(offset) = params["offset"].as_i64() {
                cmd.push_str(&format!(" --offset {offset}"));
            }
            if let Some(limit) = params["limit"].as_i64() {
                cmd.push_str(&format!(" --limit {limit}"));
            }
            Ok(cmd)
        }
        "NuShell" => {
            // Direct pass-through
            Ok(params["command"].as_str().required()?.to_string())
        }
        // ... other tools
    }
}
```

String parameters containing special characters must be escaped for nu syntax. The `quote_nu()` helper wraps values in single quotes with appropriate escaping.

### Error mapping

Nu `error make` messages and sandbox permission errors are returned to the agent as tool result text. The translation layer does not interpret or reformat errors — it passes them through so the agent sees the same error context as if it had invoked the command directly.

---

## Nu Custom Command Definitions

### Loading mechanism

At session startup, before any agent tool calls, epic sends an `evaluate` call containing all custom command definitions as nu script. This is a single MCP `tools/call` request with the `def` blocks concatenated. The definitions persist in the nu session for subsequent `evaluate` calls.

Alternative: pass definitions via `nu --commands "..." --mcp`. Needs testing — `--commands` may conflict with `--mcp` mode. If it works, it avoids the extra evaluate round-trip.

### Command definitions

Each command preserves the semantics of the current Rust implementation with Claude Code-aligned enhancements. Error reporting uses nu's structured error mechanism (`error make`).

**These are illustrative, not final.** Exact nu API calls, error handling, and output formatting need validation against nu 0.111.0.

```nu
# epic_read — read file contents, 256 KiB cap, optional line pagination
def epic_read [
    path: string
    --offset: int    # 1-based line number to start from
    --limit: int     # max lines to return
] {
    let full = ($path | path expand)
    let size = (ls $full | get size | first)
    if $size > 262144 {
        error make {
            msg: $"File too large: ($size) bytes, max 262144"
        }
    }
    let lines = (open $full --raw | decode utf-8 | lines)
    let start = if ($offset | is-empty) { 0 } else { $offset - 1 }
    let lines = ($lines | skip $start)
    let lines = if ($limit | is-empty) { $lines } else { $lines | first $limit }
    # Output with line numbers (cat -n style)
    $lines | enumerate | each { |row|
        $"($row.index + $start + 1 | fill -a right -w 6) ($row.item)"
    } | str join "\n"
}

# epic_write — write content to file, 1 MiB cap
def epic_write [path: string, content: string] {
    let size = ($content | str length)
    if $size > 1048576 {
        error make {
            msg: $"Content too large: ($size) bytes, max 1048576"
        }
    }
    let full = ($path | path expand)
    let parent = ($full | path dirname)
    mkdir $parent
    $content | save --force $full
}

# epic_edit — exact substring replacement, optional replace-all
def epic_edit [
    path: string
    old_string: string
    new_string: string
    --replace-all    # replace all occurrences instead of requiring uniqueness
] {
    let full = ($path | path expand)
    let content = (open $full --raw | decode utf-8)

    if not $replace_all {
        # Count occurrences — TBD: validate nu API for this
        let parts = ($content | split row $old_string)
        let count = (($parts | length) - 1)
        if $count == 0 {
            error make { msg: "old_string not found in file" }
        }
        if $count > 1 {
            error make {
                msg: $"old_string found ($count) times, must be unique. Use --replace-all to replace all occurrences."
            }
        }
        let result = ($content | str replace $old_string $new_string)
        $result | save --force $full
    } else {
        let result = ($content | str replace --all $old_string $new_string)
        if $result == $content {
            error make { msg: "old_string not found in file" }
        }
        $result | save --force $full
    }
}

# epic_glob — find files by pattern, 1000 result cap
def epic_glob [
    pattern: string
    --path: string   # directory to search in
] {
    let dir = if ($path | is-empty) { "." } else { $path }
    cd $dir
    glob $pattern | first 1000 | to text
}

# epic_grep — search file contents via rg, 64 KiB output cap
# Requires rg (ripgrep) binary in the sandbox.
def epic_grep [
    pattern: string
    --path: string
    --output-mode: string          # content, files_with_matches (default), count
    --glob: string                 # file name filter
    --type: string                 # file type filter (js, py, rust...)
    --case-insensitive             # case insensitive search
    --line-numbers                 # show line numbers (default true for content mode)
    --no-line-numbers              # disable line numbers
    --context-after: int           # lines after match
    --context-before: int          # lines before match
    --context: int                 # lines before and after match
    --multiline                    # match across lines
    --head-limit: int              # limit result count
] {
    let search_path = if ($path | is-empty) { "." } else { $path }
    let mode = if ($output_mode | is-empty) { "files_with_matches" } else { $output_mode }

    mut args = [$pattern]

    # Output mode
    if $mode == "files_with_matches" { $args = ($args | append "-l") }
    if $mode == "count" { $args = ($args | append "-c") }

    # Filters
    if not ($glob | is-empty) { $args = ($args | append ["--glob" $glob]) }
    if not ($type | is-empty) { $args = ($args | append ["--type" $type]) }
    if $case_insensitive { $args = ($args | append "-i") }
    if $multiline { $args = ($args | append "--multiline") }

    # Line numbers (default on for content mode)
    if $mode == "content" {
        if $no_line_numbers {
            $args = ($args | append "--no-line-number")
        } else {
            $args = ($args | append "-n")
        }
    }

    # Context lines
    if not ($context_after | is-empty) { $args = ($args | append ["-A" ($context_after | into string)]) }
    if not ($context_before | is-empty) { $args = ($args | append ["-B" ($context_before | into string)]) }
    if not ($context | is-empty) { $args = ($args | append ["-C" ($context | into string)]) }

    $args = ($args | append $search_path)

    let output = (^rg ...$args | complete)

    # Apply head_limit
    let lines = ($output.stdout | lines)
    let lines = if not ($head_limit | is-empty) { $lines | first $head_limit } else { $lines }

    # Apply 64 KiB output cap
    # TBD: truncation strategy
    $lines | str join "\n"
}
```

### Prototype validation items

1. **`epic_read` line pagination** — validate `lines | skip | first` pipeline with edge cases (offset beyond EOF, empty files).
2. **`epic_edit` substring counting** — validate `split row` approach for counting. May need literal string matching (not regex).
3. **`epic_grep` rg integration** — validate `^rg` external command invocation within lot sandbox. Requires rg binary accessible in the sandbox path.
4. **`epic_glob` cwd behavior** — confirm `cd $dir; glob $pattern` resolves correctly within sandbox.
5. **Binary file handling** — `decode utf-8` may fail on binary files. Need error handling or `--raw` fallback.
6. **Output format** — nu `evaluate` returns output as string. Verify agents can parse the formatted output correctly.
7. **`--commands` + `--mcp` compatibility** — test whether `nu --commands "def epic_read ..." --mcp` works to avoid the extra evaluate round-trip.

---

## Implementation Plan

### Phase 1: Prototype nu custom commands

1. Write and test nu custom command definitions against nu 0.111.0.
2. Validate: `--commands` + `--mcp` compatibility.
3. Validate: error reporting via `error make` surfaces correctly through MCP `evaluate` responses.
4. Validate: `epic_grep` with bundled `rg` binary inside lot sandbox.
5. Validate: output format compatibility (numbered lines, grep results, glob lists).
6. Validate: `epic_edit` substring counting and `replace_all` behavior.

### Phase 2: Tool executor translation layer

1. Add `file_tool_forwarders` config field to `EpicConfig` (default: true).
2. Implement `translate_tool_call()` in Rust: JSON tool params → nu command string.
3. Implement `quote_nu()` string escaping for safe parameter injection.
4. Update `tool_definitions()` to conditionally return Claude Code-aligned schemas (forwarders on) or NuShell-only (forwarders off).
5. Update `execute_tool()` dispatch: when forwarders on, translate → `nu_session.evaluate()`; when off, reject non-NuShell tool calls.
6. Unit test the translation layer (JSON → nu command → expected string).

### Phase 3: Session startup injection

1. Modify `NuSession::initialize()` to send custom command definitions after MCP handshake.
2. Test: commands persist across subsequent `evaluate` calls in the same session.
3. Test: command definitions don't interfere with raw NuShell commands.

### Phase 4: Remove old tool layer

1. Remove `tool_read_file`, `tool_write_file`, `tool_edit_file`, `tool_glob`, `tool_grep` from `tools.rs`.
2. Remove `safe_path()`, `verify_ancestors_within_root()`.
3. Remove `ToolGrant::READ`, `ToolGrant::WRITE`. Simplify `ToolGrant` to phase marker.
4. Rename `nu` → `NuShell` in tool definitions and dispatch.
5. Update prompt assembly (`prompts.rs`) with new tool names and descriptions.

### Phase 5: Sandbox policy consolidation and docs

1. Verify lot `read_path` prevents writes on all platforms (automated tests).
2. Verify `rg` binary is accessible within lot sandbox on all platforms.
3. Verify temp dir access cannot pivot to project root.
4. Remove any remaining `safe_path` references.
5. Update DESIGN.md and README.md to reflect unified model and new tool names.

---

## Risks

### grep implementation complexity

Current Rust `tool_grep` uses the `regex` and `walkdir` crates for recursive regex search with file-size filtering. The new `epic_grep` wraps ripgrep.

| Approach | Pros | Cons |
|---|---|---|
| Shell out to `rg` (ripgrep) | Feature-complete, fast, Claude Code also uses rg | Requires rg binary in sandbox; extra dependency |
| Nu `open` + `lines` + `find` pipeline | No extra dependency | Slow on large trees, limited regex |

**Decision**: Ship `rg` alongside `nu` in the build (same download-and-cache pattern in `build.rs`). `rg` is a single static binary, widely available, and is the engine behind Claude Code's Grep tool. The `epic_grep` command becomes a wrapper around `rg` with Claude Code-compatible output formatting.

### Parameter injection in translation layer

The `translate_tool_call()` function constructs nu command strings from JSON parameters. Malformed or adversarial parameter values could inject nu syntax. The `quote_nu()` helper must handle: single quotes, double quotes, backticks, subshell expressions (`$(...)`, `` `...` ``), null bytes, newlines. This is a correctness concern (not a security concern — the sandbox limits blast radius), but injection could cause confusing errors.

Mitigation: comprehensive unit tests for `quote_nu()` with adversarial inputs. Consider using nu's `--value` parameter passing if available in 0.111.0.

### Performance on large repositories

File tool calls gain IPC overhead (~1ms per call). For typical agent sessions (10-50 tool calls), this adds <50ms total. For pathological grep-over-large-tree cases, the bottleneck is I/O, not IPC.

### Error message fidelity

Agents rely on error messages to recover (e.g., "file not found" vs "permission denied" vs "size limit exceeded"). Nu's `error make` produces structured errors that surface through MCP `evaluate` responses. The error text must be clear enough for the agent to act on. Test this explicitly during Phase 1.

### Sandbox policy for temp dirs

Lot grants writable temp dir access on all platforms. This is necessary (nu needs temp space for internal operations). Temp dirs are outside the project root, so an agent writing to temp cannot affect project files. However, an agent could use temp as scratch space to work around read-only project root restrictions (read file → copy to temp → modify in temp). This is not a security concern (the agent can't write the result back to the project root), but it's worth noting.

### NuShell syntax adoption

The shell tool is named `NuShell` (not `Bash`) to steer models toward NuShell syntax. The description explicitly references Claude Code's Bash tool to activate the right behavioral associations (shell execution, session persistence) while the name prevents POSIX syntax generation. If models still struggle with NuShell syntax, the system prompt can include a brief NuShell syntax primer.

## Non-Goals

- Parallel nu sessions within a single agent call.
- Adding tools beyond the six defined here (Read, Write, Edit, Glob, Grep, NuShell).
- Streaming tool output to the TUI during execution.
- MCP-level tool registration in nu (blocked by nu 0.111.0 limitations).
