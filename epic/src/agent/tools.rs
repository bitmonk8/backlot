// Tool grant flags, tool definitions (legacy + forwarded), nu command translation, and execution dispatch.

use crate::agent::nu_session::{NuOutput, NuSession};
use bitflags::bitflags;
use globset::GlobBuilder;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_GLOB_RESULTS: usize = 1000;
const MAX_GREP_OUTPUT: usize = 64 * 1024;
const MAX_GREP_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_NU_OUTPUT: usize = 64 * 1024;
const DEFAULT_NU_TIMEOUT_SECS: u64 = 120;
const MAX_NU_TIMEOUT_SECS: u64 = 600;

bitflags! {
    /// Permission flags controlling which tools an agent call may use.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ToolGrant: u8 {
        const READ  = 0b0000_0001;
        const WRITE = 0b0000_0010;
        const NU    = 0b0000_0100;
    }
}

/// Method categories that map to different tool grant sets.
#[derive(Debug, Clone, Copy)]
pub enum AgentMethod {
    /// Assessment / verification / checkpoint / recovery — read-only analysis.
    Analyze,
    /// Leaf execution — needs read, write, nu.
    Execute,
    /// Design and decompose — needs read and nu for exploration.
    Decompose,
}

/// Returns the tool grant set appropriate for a given agent method.
///
/// All phases include NU because forwarded file tools route through the nu
/// session. Without NU, forwarded Read/Glob/Grep would be unavailable.
/// In legacy mode this also exposes the `nu` tool to Analyze — acceptable
/// because the lot sandbox enforces read-only access for that phase.
pub fn phase_tools(method: AgentMethod) -> ToolGrant {
    match method {
        AgentMethod::Execute => ToolGrant::READ | ToolGrant::WRITE | ToolGrant::NU,
        AgentMethod::Analyze | AgentMethod::Decompose => ToolGrant::READ | ToolGrant::NU,
    }
}

/// A tool definition suitable for inclusion in a Flick config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlickToolDef {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
}

/// Returns tool definitions for all tools permitted by the given grant.
///
/// When `file_tool_forwarders` is true, returns Claude Code-aligned schemas
/// (Read, Write, Edit, Glob, Grep, `NuShell`) that forward to nu custom commands.
/// When false, returns legacy tool schemas (`read_file`, `glob`, `grep`, `write_file`,
/// `edit_file`, `nu`) with Rust-native implementations.
pub fn tool_definitions(grant: ToolGrant, file_tool_forwarders: bool) -> Vec<FlickToolDef> {
    if file_tool_forwarders {
        return forwarded_tool_definitions(grant);
    }
    legacy_tool_definitions(grant)
}

/// Legacy tool definitions. Used when `file_tool_forwarders` = false.
fn legacy_tool_definitions(grant: ToolGrant) -> Vec<FlickToolDef> {
    let mut tools = Vec::new();

    if grant.contains(ToolGrant::READ) {
        tools.push(FlickToolDef {
            name: "read_file".into(),
            description: "Read the contents of a file at the given path.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or project-relative file path" }
                },
                "required": ["path"]
            }),
        });
        tools.push(FlickToolDef {
            name: "glob".into(),
            description: "Find files matching a glob pattern.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs)" },
                    "path": { "type": "string", "description": "Directory to search in (defaults to project root)" }
                },
                "required": ["pattern"]
            }),
        });
        tools.push(FlickToolDef {
            name: "grep".into(),
            description: "Search file contents for a regex pattern.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "File or directory to search in" },
                    "glob": { "type": "string", "description": "Glob filter for file names" }
                },
                "required": ["pattern"]
            }),
        });
    }

    if grant.contains(ToolGrant::WRITE) {
        tools.push(FlickToolDef {
            name: "write_file".into(),
            description: "Write content to a file, creating it if necessary.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to write to" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
        });
        tools.push(FlickToolDef {
            name: "edit_file".into(),
            description: "Replace a specific string in a file with new content.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to edit" },
                    "old_string": { "type": "string", "description": "Exact text to find and replace" },
                    "new_string": { "type": "string", "description": "Replacement text" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        });
    }

    if grant.contains(ToolGrant::NU) {
        tools.push(FlickToolDef {
            name: "nu".into(),
            description: "Execute a NuShell command and return its output. Uses NuShell syntax (not POSIX sh). Session state (variables, env, cwd) persists across calls.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The NuShell command to execute" },
                    "timeout": { "type": "integer", "description": "Timeout in seconds (default 120)" }
                },
                "required": ["command"]
            }),
        });
    }

    tools
}

/// Claude Code-aligned tool definitions. File tools forward to nu custom commands.
fn forwarded_tool_definitions(grant: ToolGrant) -> Vec<FlickToolDef> {
    let mut tools = Vec::new();

    // Read-only tools: available when NU is granted (all tool-granted phases)
    if grant.contains(ToolGrant::NU) {
        tools.push(FlickToolDef {
            name: "Read".into(),
            description: "Read the contents of a file. Returns lines with line numbers. For large files, use offset and limit to read specific sections.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute or project-relative file path" },
                    "offset": { "type": "integer", "description": "Line number to start reading from (1-based). Omit to start from the beginning." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return. Omit to read up to the default cap." }
                },
                "required": ["file_path"]
            }),
        });
        tools.push(FlickToolDef {
            name: "Glob".into(),
            description: "Find files matching a glob pattern. Returns matching file paths sorted by modification time.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs, src/**/*.ts)" },
                    "path": { "type": "string", "description": "Directory to search in. Defaults to project root." }
                },
                "required": ["pattern"]
            }),
        });
        tools.push(FlickToolDef {
            name: "Grep".into(),
            description: "Search file contents for a regex pattern. Powered by ripgrep.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "File or directory to search in. Defaults to project root." },
                    "output_mode": { "type": "string", "enum": ["content", "files_with_matches", "count"], "description": "Output mode. 'content' shows matching lines, 'files_with_matches' shows only file paths (default), 'count' shows match counts." },
                    "glob": { "type": "string", "description": "Glob pattern to filter files (e.g. *.js, **/*.tsx)" },
                    "include_type": { "type": "string", "description": "File type filter (e.g. js, py, rust, go). Maps to rg --type." },
                    "case_insensitive": { "type": "boolean", "description": "Case insensitive search. Default: false." },
                    "line_numbers": { "type": "boolean", "description": "Show line numbers in output. Default: true. Only applies to 'content' output mode." },
                    "context_after": { "type": "integer", "description": "Number of lines to show after each match. Only applies to 'content' output mode." },
                    "context_before": { "type": "integer", "description": "Number of lines to show before each match. Only applies to 'content' output mode." },
                    "context": { "type": "integer", "description": "Number of lines to show before and after each match. Only applies to 'content' output mode." },
                    "multiline": { "type": "boolean", "description": "Enable multiline matching (pattern can span lines). Default: false." },
                    "head_limit": { "type": "integer", "description": "Limit output to first N lines/entries." }
                },
                "required": ["pattern"]
            }),
        });
    }

    // Write tools: require both WRITE and NU (execution routes through nu session)
    if grant.contains(ToolGrant::WRITE | ToolGrant::NU) {
        tools.push(FlickToolDef {
            name: "Write".into(),
            description: "Write content to a file, creating parent directories if necessary. Overwrites existing files.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "File path to write to" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["file_path", "content"]
            }),
        });
        tools.push(FlickToolDef {
            name: "Edit".into(),
            description: "Replace an exact string match in a file. By default, old_string must appear exactly once (prevents ambiguous edits). Set replace_all to replace every occurrence.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "File path to edit" },
                    "old_string": { "type": "string", "description": "Exact text to find and replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences instead of requiring uniqueness. Default: false." }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        });
    }

    // NuShell tool: available when NU is granted
    if grant.contains(ToolGrant::NU) {
        tools.push(FlickToolDef {
            name: "NuShell".into(),
            description: "Execute a NuShell command or pipeline and return its output. Uses NuShell syntax (not POSIX sh). Session state (variables, env, cwd) persists across calls within the same task.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The NuShell command to execute" },
                    "description": { "type": "string", "description": "Brief description of what this command does" },
                    "timeout": { "type": "integer", "description": "Timeout in seconds. Default: 120, max: 600." }
                },
                "required": ["command"]
            }),
        });
    }

    tools
}

/// Result of executing a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a raw path relative to project root and verify it stays within bounds.
/// For `allow_new_file`, canonicalize the parent instead (file may not exist yet).
fn safe_path(raw: &str, project_root: &Path, allow_new_file: bool) -> Result<PathBuf, String> {
    let root_canon = project_root
        .canonicalize()
        .map_err(|e| format!("cannot resolve project root: {e}"))?;

    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        project_root.join(raw)
    };

    let resolved = if allow_new_file {
        let parent = candidate
            .parent()
            .ok_or_else(|| format!("path has no parent: {raw}"))?;
        let parent_canon = parent
            .canonicalize()
            .map_err(|e| format!("cannot resolve parent directory: {e}"))?;
        let file_name = candidate
            .file_name()
            .ok_or_else(|| format!("path has no file name: {raw}"))?;
        let resolved = parent_canon.join(file_name);

        // Guard against existing symlinks pointing outside root
        if resolved
            .symlink_metadata()
            .map(|m| m.is_symlink())
            .unwrap_or(false)
        {
            let target = resolved
                .canonicalize()
                .map_err(|e| format!("cannot resolve symlink: {e}"))?;
            if !target.starts_with(&root_canon) {
                return Err(format!(
                    "symlink target escapes project root: {}",
                    target.display()
                ));
            }
        }

        resolved
    } else {
        candidate
            .canonicalize()
            .map_err(|e| format!("cannot resolve path '{raw}': {e}"))?
    };

    if !resolved.starts_with(&root_canon) {
        return Err(format!("path escapes project root: {}", resolved.display()));
    }

    Ok(resolved)
}

/// Verify that all ancestors of `dir` up to the nearest existing directory are
/// within the project root. Prevents `create_dir_all` from creating directories
/// outside the root before `safe_path` can reject the final path.
fn verify_ancestors_within_root(dir: &Path, root_canon: &Path) -> Result<(), String> {
    let mut cursor = dir.to_path_buf();
    loop {
        if cursor.exists() {
            let canon = cursor
                .canonicalize()
                .map_err(|e| format!("cannot canonicalize ancestor: {e}"))?;
            if !canon.starts_with(root_canon) {
                return Err(format!(
                    "ancestor escapes project root: {}",
                    canon.display()
                ));
            }
            return Ok(());
        }
        if !cursor.pop() {
            return Err("reached filesystem root without finding existing ancestor".into());
        }
    }
}

/// Map tool name to the required grant flags.
/// Handles both legacy names (`read_file`, etc.) and forwarded names (Read, etc.).
/// Forwarded Write/Edit require both WRITE and NU since they execute through `nu_session`.
fn required_grant(name: &str) -> Option<ToolGrant> {
    match name {
        // Legacy tool names
        "read_file" | "glob" | "grep" => Some(ToolGrant::READ),
        "write_file" | "edit_file" => Some(ToolGrant::WRITE),
        // Forwarded tools all route through nu_session
        "Write" | "Edit" => Some(ToolGrant::WRITE | ToolGrant::NU),
        "nu" | "NuShell" | "Read" | "Glob" | "Grep" => Some(ToolGrant::NU),
        _ => None,
    }
}

/// Returns true if the tool name is a forwarded (Claude Code-aligned) tool.
fn is_forwarded_tool(name: &str) -> bool {
    matches!(name, "Read" | "Write" | "Edit" | "Glob" | "Grep" | "NuShell")
}

// ---------------------------------------------------------------------------
// Nu command translation layer
// ---------------------------------------------------------------------------

/// Escape a string for safe inclusion in a nu command.
///
/// Uses nu single-quoted strings when possible (no escape processing).
/// Falls back to nu raw string syntax (`r#'...'#`) when the string contains
/// single quotes, using enough `#` characters to avoid premature closing.
fn quote_nu(s: &str) -> String {
    if !s.contains('\'') {
        return format!("'{s}'");
    }
    let mut n = 1;
    loop {
        let closing = format!("'{}", "#".repeat(n));
        if !s.contains(&closing) {
            break;
        }
        n += 1;
    }
    let hashes = "#".repeat(n);
    format!("r{hashes}'{s}'{hashes}")
}

/// Translate a JSON tool call into a nu command string.
///
/// Appends `| to json -r` so the Rust layer can parse structured output.
/// `NuShell` is handled separately in `execute_forwarded_tool` (direct
/// pass-through to `tool_nu`).
fn translate_tool_call(name: &str, input: &JsonValue) -> Result<String, String> {
    let cmd = match name {
        "Read" => translate_read(input),
        "Write" => translate_write(input),
        "Edit" => translate_edit(input),
        "Glob" => translate_glob(input),
        "Grep" => translate_grep(input),
        _ => Err(format!("unknown forwarded tool: {name}")),
    }?;
    Ok(format!("{cmd} | to json -r"))
}

fn translate_read(input: &JsonValue) -> Result<String, String> {
    let path = get_str(input, "file_path")?;
    let mut cmd = format!("epic read {}", quote_nu(path));
    if let Some(offset) = input.get("offset").and_then(JsonValue::as_i64) {
        let _ = write!(cmd, " --offset {offset}");
    }
    if let Some(limit) = input.get("limit").and_then(JsonValue::as_i64) {
        let _ = write!(cmd, " --limit {limit}");
    }
    Ok(cmd)
}

fn translate_write(input: &JsonValue) -> Result<String, String> {
    let path = get_str(input, "file_path")?;
    let content = get_str(input, "content")?;
    Ok(format!(
        "epic write {} {}",
        quote_nu(path),
        quote_nu(content)
    ))
}

fn translate_edit(input: &JsonValue) -> Result<String, String> {
    let path = get_str(input, "file_path")?;
    let old_string = get_str(input, "old_string")?;
    let new_string = get_str(input, "new_string")?;
    let mut cmd = format!(
        "epic edit {} {} {}",
        quote_nu(path),
        quote_nu(old_string),
        quote_nu(new_string)
    );
    if input
        .get("replace_all")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        cmd.push_str(" --replace-all");
    }
    Ok(cmd)
}

fn translate_glob(input: &JsonValue) -> Result<String, String> {
    let pattern = get_str(input, "pattern")?;
    let mut cmd = format!("epic glob {}", quote_nu(pattern));
    if let Some(path) = get_str_opt(input, "path") {
        let _ = write!(cmd, " --path {}", quote_nu(path));
    }
    Ok(cmd)
}

fn translate_grep(input: &JsonValue) -> Result<String, String> {
    let pattern = get_str(input, "pattern")?;
    let mut cmd = format!("epic grep {}", quote_nu(pattern));
    if let Some(path) = get_str_opt(input, "path") {
        let _ = write!(cmd, " --path {}", quote_nu(path));
    }
    if let Some(mode) = get_str_opt(input, "output_mode") {
        let _ = write!(cmd, " --output-mode {}", quote_nu(mode));
    }
    if let Some(glob) = get_str_opt(input, "glob") {
        let _ = write!(cmd, " --glob {}", quote_nu(glob));
    }
    if let Some(t) = get_str_opt(input, "include_type") {
        let _ = write!(cmd, " --type {}", quote_nu(t));
    }
    if input
        .get("case_insensitive")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        cmd.push_str(" --case-insensitive");
    }
    if let Some(ln) = input.get("line_numbers").and_then(JsonValue::as_bool) {
        if ln {
            cmd.push_str(" --line-numbers");
        } else {
            cmd.push_str(" --no-line-numbers");
        }
    }
    if let Some(n) = input.get("context_after").and_then(JsonValue::as_i64) {
        let _ = write!(cmd, " --context-after {n}");
    }
    if let Some(n) = input.get("context_before").and_then(JsonValue::as_i64) {
        let _ = write!(cmd, " --context-before {n}");
    }
    if let Some(n) = input.get("context").and_then(JsonValue::as_i64) {
        let _ = write!(cmd, " --context {n}");
    }
    if input
        .get("multiline")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        cmd.push_str(" --multiline");
    }
    if let Some(n) = input.get("head_limit").and_then(JsonValue::as_i64) {
        let _ = write!(cmd, " --head-limit {n}");
    }
    Ok(cmd)
}

/// Format structured JSON output from a forwarded nu command into Claude-friendly text.
///
/// File tools pipe their output through `| to json -r`, so `raw_output` is JSON.
/// On parse failure, returns the raw output unchanged.
fn format_tool_result(name: &str, raw_output: &str) -> String {
    match name {
        "Read" => format_read_result(raw_output),
        "Write" => format_write_result(raw_output),
        "Edit" => format_edit_result(raw_output),
        "Glob" => format_glob_result(raw_output),
        "Grep" => format_grep_result(raw_output),
        _ => raw_output.to_owned(),
    }
}

fn format_read_result(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<JsonValue>(raw) else {
        return raw.to_owned();
    };

    let total_lines = v["total_lines"].as_u64().unwrap_or(0);
    let offset = v["offset"].as_u64().unwrap_or(1);
    let lines_returned = v["lines_returned"].as_u64().unwrap_or(0);
    let size = v["size"].as_u64().unwrap_or(0);

    let mut output = String::new();
    if let Some(lines) = v["lines"].as_array() {
        for entry in lines {
            let line_num = entry["line"].as_u64().unwrap_or(0);
            let text = entry["text"].as_str().unwrap_or("");
            let _ = writeln!(output, "{line_num:>6}\t{text}");
        }
    }

    if total_lines > 0 && lines_returned > 0 {
        let end = offset + lines_returned - 1;
        let _ = write!(
            output,
            "(showing lines {offset}-{end} of {total_lines} total, {size} bytes)"
        );
    } else if total_lines > 0 {
        let _ = write!(output, "(0 lines returned, {total_lines} total, {size} bytes)");
    }

    output
}

fn format_write_result(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<JsonValue>(raw) else {
        return raw.to_owned();
    };
    let path = v["path"].as_str().unwrap_or("?");
    let bytes = v["bytes_written"].as_u64().unwrap_or(0);
    format!("Wrote {bytes} bytes to {path}")
}

fn format_edit_result(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<JsonValue>(raw) else {
        return raw.to_owned();
    };
    let path = v["path"].as_str().unwrap_or("?");
    let count = v["replacements"].as_u64().unwrap_or(0);
    let s = if count == 1 { "" } else { "s" };
    format!("Replaced {count} occurrence{s} in {path}")
}

fn format_glob_result(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<JsonValue>(raw) else {
        return raw.to_owned();
    };
    v.as_array().map_or_else(
        || raw.to_owned(),
        |arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        },
    )
}

fn format_grep_result(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<JsonValue>(raw) else {
        return raw.to_owned();
    };
    v["output"].as_str().unwrap_or(raw).to_owned()
}

fn get_str<'a>(input: &'a JsonValue, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("missing or non-string parameter: {key}"))
}

fn get_str_opt<'a>(input: &'a JsonValue, key: &str) -> Option<&'a str> {
    input.get(key).and_then(JsonValue::as_str)
}

/// Execute a tool call, checking grants and dispatching to the implementation.
///
/// Handles both legacy tool names (`read_file`, `write_file`, etc.) and forwarded
/// Claude Code-aligned names (Read, Write, etc.). Forwarded tools are translated
/// into nu commands and dispatched through `nu_session.evaluate()`.
pub async fn execute_tool(
    tool_use_id: String,
    name: &str,
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
    nu_session: &NuSession,
) -> ToolExecResult {
    // Check grant
    match required_grant(name) {
        Some(needed) if !grant.contains(needed) => {
            return ToolExecResult {
                tool_use_id,
                content: format!("tool '{name}' not permitted by current grant"),
                is_error: true,
            };
        }
        None => {
            return ToolExecResult {
                tool_use_id,
                content: format!("unknown tool: {name}"),
                is_error: true,
            };
        }
        Some(_) => {}
    }

    // Forwarded tools: translate JSON params → nu command → nu_session.evaluate()
    if is_forwarded_tool(name) {
        return execute_forwarded_tool(tool_use_id, name, input, project_root, grant, nu_session)
            .await;
    }

    // Legacy nu tool
    if name == "nu" {
        return nu_result_to_exec(
            tool_use_id,
            tool_nu(input, project_root, grant, nu_session).await,
        );
    }

    // Legacy file tools (Rust implementations, to be removed in Phase 4)
    let result = match name {
        "read_file" => tool_read_file(input, project_root).await,
        "glob" => tool_glob(input, project_root).await,
        "grep" => tool_grep(input, project_root).await,
        "write_file" => tool_write_file(input, project_root).await,
        "edit_file" => tool_edit_file(input, project_root).await,
        _ => unreachable!("required_grant() filters unknown tools before dispatch"),
    };

    match result {
        Ok(content) => ToolExecResult {
            tool_use_id,
            content,
            is_error: false,
        },
        Err(msg) => ToolExecResult {
            tool_use_id,
            content: msg,
            is_error: true,
        },
    }
}

/// Convert a `tool_nu` result into a `ToolExecResult`.
fn nu_result_to_exec(tool_use_id: String, result: Result<NuOutput, String>) -> ToolExecResult {
    match result {
        Ok(out) => ToolExecResult {
            tool_use_id,
            content: out.content,
            is_error: out.is_error,
        },
        Err(msg) => ToolExecResult {
            tool_use_id,
            content: msg,
            is_error: true,
        },
    }
}

/// Execute a forwarded (Claude Code-aligned) tool via the nu session.
async fn execute_forwarded_tool(
    tool_use_id: String,
    name: &str,
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
    nu_session: &NuSession,
) -> ToolExecResult {
    // NuShell is a direct pass-through (same as legacy "nu" tool)
    if name == "NuShell" {
        return nu_result_to_exec(
            tool_use_id,
            tool_nu(input, project_root, grant, nu_session).await,
        );
    }

    // Translate JSON tool params to nu command string
    let nu_command = match translate_tool_call(name, input) {
        Ok(cmd) => cmd,
        Err(msg) => {
            return ToolExecResult {
                tool_use_id,
                content: msg,
                is_error: true,
            };
        }
    };

    // Execute via nu session, then format+truncate successful output.
    // Empty output is valid (Glob/Grep with no matches), so don't replace it.
    let result = nu_session
        .evaluate(&nu_command, DEFAULT_NU_TIMEOUT_SECS, project_root, grant)
        .await
        .map(|out| {
            if out.is_error {
                out
            } else {
                NuOutput {
                    content: truncate_output(format_tool_result(name, &out.content)),
                    is_error: false,
                }
            }
        });

    nu_result_to_exec(tool_use_id, result)
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

async fn tool_read_file(input: &JsonValue, project_root: &Path) -> Result<String, String> {
    let raw_path = get_str(input, "path")?;
    let path = safe_path(raw_path, project_root, false)?;

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("cannot read file: {e}"))?;

    if !meta.is_file() {
        return Err(format!("not a file: {}", path.display()));
    }

    let size = meta.len();
    if size > MAX_READ_BYTES {
        use tokio::io::AsyncReadExt;
        let limit = usize::try_from(MAX_READ_BYTES).unwrap_or(usize::MAX);
        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| format!("read error: {e}"))?;
        let mut buf = vec![0u8; limit];
        file.read_exact(&mut buf)
            .await
            .map_err(|e| format!("read error: {e}"))?;
        let truncated = String::from_utf8_lossy(&buf);
        return Ok(format!(
            "{truncated}\n\n[truncated: file is {size} bytes, showed first {MAX_READ_BYTES}]"
        ));
    }

    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read error: {e}"))
}

async fn tool_glob(input: &JsonValue, project_root: &Path) -> Result<String, String> {
    let pattern = get_str(input, "pattern")?.to_owned();
    let search_dir = match get_str_opt(input, "path") {
        Some(p) => safe_path(p, project_root, false)?,
        None => project_root.to_path_buf(),
    };

    let matcher = GlobBuilder::new(&pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| format!("invalid glob pattern: {e}"))?
        .compile_matcher();

    tokio::task::spawn_blocking(move || {
        let mut results = Vec::new();
        for entry in WalkDir::new(&search_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if results.len() >= MAX_GLOB_RESULTS {
                break;
            }
            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(&search_dir) {
                let rel_str = rel.to_string_lossy();
                if !rel_str.is_empty() && matcher.is_match(&*rel_str) {
                    results.push(rel_str.into_owned());
                }
            }
        }

        if results.len() >= MAX_GLOB_RESULTS {
            results.push(format!("[truncated at {MAX_GLOB_RESULTS} results]"));
        }

        Ok(results.join("\n"))
    })
    .await
    .map_err(|e| format!("glob task failed: {e}"))?
}

async fn tool_grep(input: &JsonValue, project_root: &Path) -> Result<String, String> {
    let pattern = get_str(input, "pattern")?.to_owned();
    let search_dir = match get_str_opt(input, "path") {
        Some(p) => safe_path(p, project_root, false)?,
        None => project_root.to_path_buf(),
    };

    let re = RegexBuilder::new(&pattern)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|e| format!("invalid regex: {e}"))?;

    let glob_filter = match get_str_opt(input, "glob") {
        Some(g) => Some(
            GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| format!("invalid glob filter: {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    tokio::task::spawn_blocking(move || {
        let mut output = String::new();

        for entry in WalkDir::new(&search_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if output.len() >= MAX_GREP_OUTPUT {
                let _ = write!(output, "\n[truncated at {MAX_GREP_OUTPUT} bytes]");
                break;
            }

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Skip files larger than 10 MiB
            let Ok(meta) = path.metadata() else {
                continue;
            };
            if meta.len() > MAX_GREP_FILE_BYTES {
                continue;
            }

            // Apply glob filter on relative path
            if let Some(ref gf) = glob_filter {
                let Ok(rel) = path.strip_prefix(&search_dir) else {
                    continue;
                };
                if !gf.is_match(&*rel.to_string_lossy()) {
                    continue;
                }
            }

            // Read file, skip binary/unreadable
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };

            // Skip binary files (null bytes in first 8 KiB)
            let check_len = bytes.len().min(8192);
            if bytes[..check_len].contains(&0) {
                continue;
            }

            let Ok(content) = std::str::from_utf8(&bytes) else {
                continue;
            };

            let rel = path.strip_prefix(&search_dir).unwrap_or(path);
            for (line_num, line) in content.lines().enumerate() {
                if output.len() >= MAX_GREP_OUTPUT {
                    break;
                }
                if re.is_match(line) {
                    let _ = writeln!(output, "{}:{}:{}", rel.display(), line_num + 1, line);
                }
            }
        }

        Ok(output)
    })
    .await
    .map_err(|e| format!("grep task failed: {e}"))?
}

async fn tool_write_file(input: &JsonValue, project_root: &Path) -> Result<String, String> {
    let raw_path = get_str(input, "path")?;
    let content = get_str(input, "content")?;

    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "content too large: {} bytes (limit: {MAX_WRITE_BYTES})",
            content.len()
        ));
    }

    // Resolve path: for new files, create parent dirs first, then validate containment.
    let candidate = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        project_root.join(raw_path)
    };

    let parent = candidate
        .parent()
        .ok_or_else(|| format!("path has no parent: {raw_path}"))?;

    // Verify ancestors are within root BEFORE creating directories
    if !parent.exists() {
        let root_canon = project_root
            .canonicalize()
            .map_err(|e| format!("cannot resolve project root: {e}"))?;
        verify_ancestors_within_root(parent, &root_canon)?;
    }

    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("cannot create directory: {e}"))?;

    let path = safe_path(raw_path, project_root, true)?;

    let bytes = content.as_bytes();
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| format!("write error: {e}"))?;

    Ok(format!("wrote {} bytes to {}", bytes.len(), path.display()))
}

async fn tool_edit_file(input: &JsonValue, project_root: &Path) -> Result<String, String> {
    let raw_path = get_str(input, "path")?;
    let old_string = get_str(input, "old_string")?;
    let new_string = get_str(input, "new_string")?;
    let path = safe_path(raw_path, project_root, false)?;

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read error: {e}"))?;

    let count = content.matches(old_string).count();
    if count == 0 {
        return Err(format!("old_string not found in {}", path.display()));
    }
    if count > 1 {
        return Err(format!(
            "old_string found {count} times in {} (must be unique)",
            path.display()
        ));
    }

    let new_content = content.replacen(old_string, new_string, 1);
    tokio::fs::write(&path, &new_content)
        .await
        .map_err(|e| format!("write error: {e}"))?;

    Ok(format!("edited {}", path.display()))
}

async fn tool_nu(
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
    nu_session: &NuSession,
) -> Result<NuOutput, String> {
    let command = get_str(input, "command")?;
    let timeout_secs = input
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .unwrap_or(DEFAULT_NU_TIMEOUT_SECS)
        .min(MAX_NU_TIMEOUT_SECS);

    let mut result = nu_session
        .evaluate(command, timeout_secs, project_root, grant)
        .await?;

    result.content = format_nu_output(result.content);

    Ok(result)
}

/// Truncate output to `MAX_NU_OUTPUT` without replacing empty strings.
/// Used for forwarded tool results where empty output is semantically valid.
fn truncate_output(raw: String) -> String {
    if raw.len() > MAX_NU_OUTPUT {
        let mut output = raw;
        let mut end = MAX_NU_OUTPUT;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        output.push_str("\n[output truncated]");
        output
    } else {
        raw
    }
}

fn format_nu_output(raw: String) -> String {
    if raw.is_empty() {
        return "[no output]".into();
    }
    truncate_output(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: execute a tool in a fresh temp dir (for tests that don't need
    /// pre-populated files).
    async fn exec(name: &str, input: serde_json::Value, grant: ToolGrant) -> ToolExecResult {
        let tmp = TempDir::new().unwrap();
        let session = NuSession::new();
        execute_tool("tu_1".into(), name, &input, tmp.path(), grant, &session).await
    }

    /// Helper: execute a tool in a specific directory.
    async fn exec_in(
        name: &str,
        input: serde_json::Value,
        path: &std::path::Path,
        grant: ToolGrant,
    ) -> ToolExecResult {
        let session = NuSession::new();
        execute_tool("tu_1".into(), name, &input, path, grant, &session).await
    }

    #[test]
    fn analyze_gets_read_nu() {
        let grant = phase_tools(AgentMethod::Analyze);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(grant.contains(ToolGrant::NU));
    }

    #[test]
    fn decompose_gets_read_nu() {
        let grant = phase_tools(AgentMethod::Decompose);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(grant.contains(ToolGrant::NU));
    }

    #[test]
    fn execute_gets_read_write_nu() {
        let grant = phase_tools(AgentMethod::Execute);
        assert!(grant.contains(ToolGrant::READ));
        assert!(grant.contains(ToolGrant::WRITE));
        assert!(grant.contains(ToolGrant::NU));
    }

    #[test]
    fn read_only_tools() {
        let tools = tool_definitions(ToolGrant::READ, false);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"nu"));
    }

    #[test]
    fn full_tools() {
        let grant = ToolGrant::READ | ToolGrant::WRITE | ToolGrant::NU;
        let tools = tool_definitions(grant, false);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"nu"));
    }

    #[test]
    fn empty_grant_no_tools() {
        let tools = tool_definitions(ToolGrant::empty(), false);
        assert!(tools.is_empty());
    }

    // -- safe_path tests --

    #[test]
    fn test_safe_path_relative() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "hi").unwrap();
        let resolved = safe_path("hello.txt", tmp.path(), false).unwrap();
        assert!(resolved.ends_with("hello.txt"));
        assert!(resolved.starts_with(tmp.path().canonicalize().unwrap()));
    }

    #[test]
    fn test_safe_path_traversal_blocked() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        let result = safe_path("../../../etc/passwd", tmp.path(), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_safe_path_absolute_outside() {
        let tmp = TempDir::new().unwrap();
        // Use the temp dir's parent as an "outside" absolute path
        let outside = tmp.path().parent().unwrap().join("outside.txt");
        // Create the file so canonicalize works
        let _ = std::fs::write(&outside, "");
        let result = safe_path(outside.to_str().unwrap(), tmp.path(), false);
        // Clean up
        let _ = std::fs::remove_file(&outside);
        assert!(result.is_err());
    }

    // -- grant check tests --

    #[tokio::test]
    async fn test_grant_check_denies() {
        let result = exec(
            "write_file",
            serde_json::json!({"path": "x.txt", "content": "hi"}),
            ToolGrant::READ, // no WRITE
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not permitted"));
    }

    #[tokio::test]
    async fn test_grant_check_unknown_tool() {
        let result = exec(
            "nonexistent_tool",
            serde_json::json!({}),
            ToolGrant::READ | ToolGrant::WRITE | ToolGrant::NU,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown tool"));
    }

    // -- read_file tests --

    #[tokio::test]
    async fn test_read_file_basic() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world").unwrap();
        let result = exec_in(
            "read_file",
            serde_json::json!({"path": "test.txt"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "hello world");
    }

    #[tokio::test]
    async fn test_read_file_truncation() {
        let tmp = TempDir::new().unwrap();
        // Write a file larger than MAX_READ_BYTES
        let big = "x".repeat(usize::try_from(MAX_READ_BYTES).unwrap() + 100);
        std::fs::write(tmp.path().join("big.txt"), &big).unwrap();
        let result = exec_in(
            "read_file",
            serde_json::json!({"path": "big.txt"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("[truncated"));
    }

    #[tokio::test]
    async fn test_read_file_nonexistent() {
        let result = exec(
            "read_file",
            serde_json::json!({"path": "nope.txt"}),
            ToolGrant::READ,
        )
        .await;
        assert!(result.is_error);
    }

    // -- glob tests --

    #[tokio::test]
    async fn test_glob_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(tmp.path().join("readme.md"), "").unwrap();
        let result = exec_in(
            "glob",
            serde_json::json!({"pattern": "**/*.rs"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("lib.rs"));
        assert!(!result.content.contains("readme.md"));
    }

    // -- grep tests --

    #[tokio::test]
    async fn test_grep_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("code.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("data.txt"), "no match here\n").unwrap();
        let result = exec_in(
            "grep",
            serde_json::json!({"pattern": "fn main"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("code.rs"));
        assert!(result.content.contains("fn main"));
        assert!(!result.content.contains("data.txt"));
    }

    #[tokio::test]
    async fn test_grep_regex_complexity_rejected() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "aaa\n").unwrap();
        let huge = format!("({}){{{}}}", "a|b|c|d|e|f|g|h|i|j".repeat(50), 9999);
        let result = exec_in(
            "grep",
            serde_json::json!({"pattern": huge}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid regex"));
    }

    #[tokio::test]
    async fn test_grep_normal_regex_works() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("code.rs"), "fn hello_world() {}\n").unwrap();
        let result = exec_in(
            "grep",
            serde_json::json!({"pattern": r"fn\s+\w+"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("fn hello_world"));
    }

    #[tokio::test]
    async fn test_grep_glob_filter_strip_prefix_skip() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn hello()\n").unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "fn hello()\n").unwrap();
        std::fs::write(tmp.path().join("src/data.txt"), "fn hello()\n").unwrap();
        let result = exec_in(
            "grep",
            serde_json::json!({"pattern": "fn hello", "glob": "*.rs"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("lib.rs"));
        assert!(!result.content.contains("data.txt"));
    }

    // -- write_file tests --

    #[tokio::test]
    async fn test_write_file_creates() {
        let tmp = TempDir::new().unwrap();
        let result = exec_in(
            "write_file",
            serde_json::json!({"path": "sub/new.txt", "content": "created"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("wrote"));
        let content = std::fs::read_to_string(tmp.path().join("sub/new.txt")).unwrap();
        assert_eq!(content, "created");
    }

    #[tokio::test]
    async fn test_write_file_size_limit() {
        let big = "x".repeat(MAX_WRITE_BYTES + 1);
        let result = exec(
            "write_file",
            serde_json::json!({"path": "big.txt", "content": big}),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("content too large"));
    }

    #[tokio::test]
    async fn test_write_file_at_limit() {
        let exact = "x".repeat(MAX_WRITE_BYTES);
        let result = exec(
            "write_file",
            serde_json::json!({"path": "exact.txt", "content": exact}),
            ToolGrant::WRITE,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("wrote"));
    }

    // -- edit_file tests --

    #[tokio::test]
    async fn test_edit_file_replaces() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello world").unwrap();
        let result = exec_in(
            "edit_file",
            serde_json::json!({"path": "f.txt", "old_string": "world", "new_string": "rust"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(!result.is_error);
        let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
        assert_eq!(content, "hello rust");
    }

    #[tokio::test]
    async fn test_edit_file_ambiguous() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "aaa aaa").unwrap();
        let result = exec_in(
            "edit_file",
            serde_json::json!({"path": "f.txt", "old_string": "aaa", "new_string": "bbb"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("2 times"));
    }

    // -- forwarded tool definitions tests --

    #[test]
    fn forwarded_read_only_tools() {
        let tools = tool_definitions(ToolGrant::READ | ToolGrant::NU, true);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Glob"));
        assert!(names.contains(&"Grep"));
        assert!(names.contains(&"NuShell"));
        assert!(!names.contains(&"Write"));
        assert!(!names.contains(&"Edit"));
    }

    #[test]
    fn forwarded_full_tools() {
        let grant = ToolGrant::READ | ToolGrant::WRITE | ToolGrant::NU;
        let tools = tool_definitions(grant, true);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Write"));
        assert!(names.contains(&"Edit"));
        assert!(names.contains(&"Glob"));
        assert!(names.contains(&"Grep"));
        assert!(names.contains(&"NuShell"));
    }

    #[test]
    fn forwarded_false_returns_legacy() {
        let tools = tool_definitions(ToolGrant::READ, false);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"Read"));
    }

    #[test]
    fn forwarded_nu_only_no_read() {
        // READ without NU: forwarded mode offers nothing (file tools need NU)
        let tools = tool_definitions(ToolGrant::READ, true);
        assert!(tools.is_empty());
    }

    // -- quote_nu tests --

    #[test]
    fn test_quote_nu_simple() {
        assert_eq!(quote_nu("hello"), "'hello'");
    }

    #[test]
    fn test_quote_nu_with_spaces() {
        assert_eq!(quote_nu("hello world"), "'hello world'");
    }

    #[test]
    fn test_quote_nu_with_single_quote() {
        let result = quote_nu("it's");
        assert_eq!(result, r"r#'it's'#");
    }

    #[test]
    fn test_quote_nu_with_single_quote_and_hash() {
        // String contains '# which is the r#'...'# closing delimiter
        let result = quote_nu("foo'#bar");
        assert_eq!(result, r"r##'foo'#bar'##");
    }

    #[test]
    fn test_quote_nu_double_quotes_no_escape() {
        // Double quotes inside single-quoted string need no escaping
        assert_eq!(quote_nu(r#"say "hi""#), r#"'say "hi"'"#);
    }

    #[test]
    fn test_quote_nu_dollar_sign() {
        // $ is inert in single-quoted strings
        assert_eq!(quote_nu("$env.PATH"), "'$env.PATH'");
    }

    #[test]
    fn test_quote_nu_newlines() {
        assert_eq!(quote_nu("line1\nline2"), "'line1\nline2'");
    }

    #[test]
    fn test_quote_nu_empty() {
        assert_eq!(quote_nu(""), "''");
    }

    #[test]
    fn test_quote_nu_backticks() {
        assert_eq!(quote_nu("`cmd`"), "'`cmd`'");
    }

    #[test]
    fn test_quote_nu_backslashes() {
        // Backslashes are literal in nu single-quoted strings (important for Windows paths)
        assert_eq!(quote_nu(r"C:\Users\foo"), r"'C:\Users\foo'");
    }

    #[test]
    fn test_quote_nu_backslash_and_single_quote() {
        let result = quote_nu(r"C:\it's");
        assert_eq!(result, r"r#'C:\it's'#");
    }

    #[test]
    fn test_quote_nu_raw_string_open_sequence() {
        // Input containing ' triggers raw string; r#' in input has no '# substring so r#'...'# works
        let result = quote_nu("r#'hello");
        assert_eq!(result, "r#'r#'hello'#");
    }

    #[test]
    fn test_quote_nu_closing_delimiter_in_input() {
        // Input containing '# forces bump to r##'...'##
        let result = quote_nu("x'#y");
        assert_eq!(result, "r##'x'#y'##");
    }

    // -- translate_tool_call tests --

    #[test]
    fn test_translate_read_basic() {
        let input = serde_json::json!({"file_path": "src/main.rs"});
        let cmd = translate_tool_call("Read", &input).unwrap();
        assert_eq!(cmd, "epic read 'src/main.rs' | to json -r");
    }

    #[test]
    fn test_translate_read_with_offset_limit() {
        let input = serde_json::json!({"file_path": "f.txt", "offset": 10, "limit": 50});
        let cmd = translate_tool_call("Read", &input).unwrap();
        assert_eq!(cmd, "epic read 'f.txt' --offset 10 --limit 50 | to json -r");
    }

    #[test]
    fn test_translate_read_missing_path() {
        let input = serde_json::json!({});
        assert!(translate_tool_call("Read", &input).is_err());
    }

    #[test]
    fn test_translate_write_basic() {
        let input = serde_json::json!({"file_path": "out.txt", "content": "hello"});
        let cmd = translate_tool_call("Write", &input).unwrap();
        assert_eq!(cmd, "epic write 'out.txt' 'hello' | to json -r");
    }

    #[test]
    fn test_translate_write_special_chars() {
        let input = serde_json::json!({"file_path": "f.txt", "content": "it's a \"test\""});
        let cmd = translate_tool_call("Write", &input).unwrap();
        assert!(cmd.starts_with("epic write 'f.txt' r#'it's a \"test\"'#"));
    }

    #[test]
    fn test_translate_edit_basic() {
        let input = serde_json::json!({
            "file_path": "f.txt",
            "old_string": "old",
            "new_string": "new"
        });
        let cmd = translate_tool_call("Edit", &input).unwrap();
        assert_eq!(cmd, "epic edit 'f.txt' 'old' 'new' | to json -r");
    }

    #[test]
    fn test_translate_edit_replace_all() {
        let input = serde_json::json!({
            "file_path": "f.txt",
            "old_string": "old",
            "new_string": "new",
            "replace_all": true
        });
        let cmd = translate_tool_call("Edit", &input).unwrap();
        assert_eq!(
            cmd,
            "epic edit 'f.txt' 'old' 'new' --replace-all | to json -r"
        );
    }

    #[test]
    fn test_translate_edit_replace_all_false() {
        let input = serde_json::json!({
            "file_path": "f.txt",
            "old_string": "old",
            "new_string": "new",
            "replace_all": false
        });
        let cmd = translate_tool_call("Edit", &input).unwrap();
        assert_eq!(cmd, "epic edit 'f.txt' 'old' 'new' | to json -r");
    }

    #[test]
    fn test_translate_glob_basic() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        let cmd = translate_tool_call("Glob", &input).unwrap();
        assert_eq!(cmd, "epic glob '**/*.rs' | to json -r");
    }

    #[test]
    fn test_translate_glob_with_path() {
        let input = serde_json::json!({"pattern": "*.txt", "path": "src"});
        let cmd = translate_tool_call("Glob", &input).unwrap();
        assert_eq!(cmd, "epic glob '*.txt' --path 'src' | to json -r");
    }

    #[test]
    fn test_translate_grep_basic() {
        let input = serde_json::json!({"pattern": "fn main"});
        let cmd = translate_tool_call("Grep", &input).unwrap();
        assert_eq!(cmd, "epic grep 'fn main' | to json -r");
    }

    #[test]
    fn test_translate_grep_full_params() {
        let input = serde_json::json!({
            "pattern": "TODO",
            "path": "src",
            "output_mode": "content",
            "glob": "*.rs",
            "include_type": "rust",
            "case_insensitive": true,
            "line_numbers": false,
            "context_after": 2,
            "context_before": 1,
            "multiline": true,
            "head_limit": 100
        });
        let cmd = translate_tool_call("Grep", &input).unwrap();
        assert!(cmd.contains("--path 'src'"));
        assert!(cmd.contains("--output-mode 'content'"));
        assert!(cmd.contains("--glob '*.rs'"));
        assert!(cmd.contains("--type 'rust'"));
        assert!(cmd.contains("--case-insensitive"));
        assert!(cmd.contains("--no-line-numbers"));
        assert!(cmd.contains("--context-after 2"));
        assert!(cmd.contains("--context-before 1"));
        assert!(cmd.contains("--multiline"));
        assert!(cmd.contains("--head-limit 100"));
    }

    #[test]
    fn test_translate_grep_context_param() {
        let input = serde_json::json!({"pattern": "x", "context": 3});
        let cmd = translate_tool_call("Grep", &input).unwrap();
        assert!(cmd.contains("--context 3"));
    }

    #[test]
    fn test_translate_grep_line_numbers_true() {
        let input = serde_json::json!({"pattern": "x", "line_numbers": true});
        let cmd = translate_tool_call("Grep", &input).unwrap();
        assert!(cmd.contains("--line-numbers"));
        assert!(!cmd.contains("--no-line-numbers"));
    }

    #[test]
    fn test_translate_nushell_not_handled() {
        // NuShell is handled directly in execute_forwarded_tool, not translate_tool_call
        let input = serde_json::json!({"command": "ls | length"});
        assert!(translate_tool_call("NuShell", &input).is_err());
    }

    #[test]
    fn test_translate_unknown_tool() {
        let input = serde_json::json!({});
        assert!(translate_tool_call("Unknown", &input).is_err());
    }

    // -- format_tool_result tests --

    #[test]
    fn test_format_read_result() {
        let json = serde_json::json!({
            "path": "/project/src/main.rs",
            "size": 256,
            "total_lines": 10,
            "offset": 1,
            "lines_returned": 3,
            "lines": [
                {"line": 1, "text": "fn main() {"},
                {"line": 2, "text": "    println!(\"hello\");"},
                {"line": 3, "text": "}"}
            ]
        });
        let result = format_read_result(&json.to_string());
        assert!(result.contains("     1\tfn main() {"));
        assert!(result.contains("     2\t    println!(\"hello\");"));
        assert!(result.contains("     3\t}"));
        assert!(result.contains("showing lines 1-3 of 10 total, 256 bytes"));
    }

    #[test]
    fn test_format_write_result() {
        let json = serde_json::json!({"path": "/project/out.txt", "bytes_written": 42});
        let result = format_write_result(&json.to_string());
        assert_eq!(result, "Wrote 42 bytes to /project/out.txt");
    }

    #[test]
    fn test_format_edit_result_singular() {
        let json = serde_json::json!({"path": "/project/f.txt", "replacements": 1});
        let result = format_edit_result(&json.to_string());
        assert_eq!(result, "Replaced 1 occurrence in /project/f.txt");
    }

    #[test]
    fn test_format_edit_result_plural() {
        let json = serde_json::json!({"path": "/project/f.txt", "replacements": 3});
        let result = format_edit_result(&json.to_string());
        assert_eq!(result, "Replaced 3 occurrences in /project/f.txt");
    }

    #[test]
    fn test_format_glob_result() {
        let json = serde_json::json!(["src/main.rs", "src/lib.rs"]);
        let result = format_glob_result(&json.to_string());
        assert_eq!(result, "src/main.rs\nsrc/lib.rs");
    }

    #[test]
    fn test_format_grep_result() {
        let json = serde_json::json!({"exit_code": 0, "output": "src/main.rs:1:fn main()"});
        let result = format_grep_result(&json.to_string());
        assert_eq!(result, "src/main.rs:1:fn main()");
    }

    #[test]
    fn test_format_grep_no_matches() {
        let json = serde_json::json!({"exit_code": 1, "output": ""});
        let result = format_grep_result(&json.to_string());
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_result_invalid_json_passthrough() {
        let raw = "not json at all";
        assert_eq!(format_read_result(raw), raw);
        assert_eq!(format_write_result(raw), raw);
        assert_eq!(format_edit_result(raw), raw);
        assert_eq!(format_glob_result(raw), raw);
        assert_eq!(format_grep_result(raw), raw);
    }

    #[test]
    fn test_format_read_result_offset_gt_1() {
        let json = serde_json::json!({
            "path": "/project/big.rs",
            "size": 5000,
            "total_lines": 200,
            "offset": 50,
            "lines_returned": 2,
            "lines": [
                {"line": 50, "text": "    let x = 1;"},
                {"line": 51, "text": "    let y = 2;"}
            ]
        });
        let result = format_read_result(&json.to_string());
        assert!(result.contains("    50\t    let x = 1;"));
        assert!(result.contains("    51\t    let y = 2;"));
        assert!(result.contains("showing lines 50-51 of 200 total, 5000 bytes"));
    }

    #[test]
    fn test_format_read_result_empty_file() {
        let json = serde_json::json!({
            "path": "/project/empty.txt",
            "size": 0,
            "total_lines": 0,
            "offset": 1,
            "lines_returned": 0,
            "lines": []
        });
        let result = format_read_result(&json.to_string());
        // total_lines=0, so no metadata line is emitted — output is empty
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_read_result_missing_lines_field() {
        let json = serde_json::json!({"error": "file not found"});
        let result = format_read_result(&json.to_string());
        // No lines to format, no metadata condition met — empty output
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_glob_result_empty_array() {
        assert_eq!(format_glob_result("[]"), "");
    }

    #[test]
    fn test_format_glob_result_non_string_elements() {
        let json = serde_json::json!([1, "a.rs", null]);
        let result = format_glob_result(&json.to_string());
        assert_eq!(result, "a.rs");
    }

    // -- phase_tools × file_tool_forwarders matrix --

    #[test]
    fn phase_forwarder_matrix() {
        // Verify the tool names produced for each phase+forwarder combination.
        for method in [AgentMethod::Analyze, AgentMethod::Decompose, AgentMethod::Execute] {
            let grant = phase_tools(method);

            // Forwarded mode
            let fwd = tool_definitions(grant, true);
            let fwd_names: Vec<&str> = fwd.iter().map(|t| t.name.as_str()).collect();
            // All phases get read tools + NuShell in forwarded mode
            assert!(fwd_names.contains(&"Read"), "{method:?} forwarded missing Read");
            assert!(fwd_names.contains(&"NuShell"), "{method:?} forwarded missing NuShell");
            // Only Execute gets Write/Edit
            if matches!(method, AgentMethod::Execute) {
                assert!(fwd_names.contains(&"Write"), "Execute forwarded missing Write");
                assert!(fwd_names.contains(&"Edit"), "Execute forwarded missing Edit");
            } else {
                assert!(!fwd_names.contains(&"Write"), "{method:?} forwarded has Write");
                assert!(!fwd_names.contains(&"Edit"), "{method:?} forwarded has Edit");
            }

            // Legacy mode
            let leg = tool_definitions(grant, false);
            let leg_names: Vec<&str> = leg.iter().map(|t| t.name.as_str()).collect();
            assert!(leg_names.contains(&"read_file"), "{method:?} legacy missing read_file");
        }
    }

    // -- required_grant for forwarded names --

    #[test]
    fn test_required_grant_forwarded_names() {
        assert_eq!(required_grant("Read"), Some(ToolGrant::NU));
        assert_eq!(required_grant("Glob"), Some(ToolGrant::NU));
        assert_eq!(required_grant("Grep"), Some(ToolGrant::NU));
        assert_eq!(required_grant("Write"), Some(ToolGrant::WRITE | ToolGrant::NU));
        assert_eq!(required_grant("Edit"), Some(ToolGrant::WRITE | ToolGrant::NU));
        assert_eq!(required_grant("NuShell"), Some(ToolGrant::NU));
    }

    #[test]
    fn test_is_forwarded_tool() {
        assert!(is_forwarded_tool("Read"));
        assert!(is_forwarded_tool("Write"));
        assert!(is_forwarded_tool("NuShell"));
        assert!(!is_forwarded_tool("read_file"));
        assert!(!is_forwarded_tool("nu"));
        assert!(!is_forwarded_tool("Xyzzy"));
        assert!(!is_forwarded_tool(""));
    }

    // -- execute_tool forwarded grant check --

    #[tokio::test]
    async fn test_forwarded_write_denied_without_grant() {
        let result = exec(
            "Write",
            serde_json::json!({"file_path": "x.txt", "content": "hi"}),
            ToolGrant::NU, // NU but no WRITE
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not permitted"));
    }

    #[tokio::test]
    async fn test_forwarded_read_denied_without_nu() {
        let result = exec(
            "Read",
            serde_json::json!({"file_path": "x.txt"}),
            ToolGrant::READ, // READ but no NU
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not permitted"));
    }

    // -- truncate_output tests --

    #[test]
    fn test_truncate_output_under_limit() {
        let s = "hello".to_owned();
        assert_eq!(truncate_output(s), "hello");
    }

    #[test]
    fn test_truncate_output_over_limit() {
        let big = "x".repeat(MAX_NU_OUTPUT + 100);
        let result = truncate_output(big);
        assert!(result.ends_with("[output truncated]"));
        assert!(result.len() <= MAX_NU_OUTPUT + 20);
    }

    #[test]
    fn test_truncate_output_empty_preserved() {
        assert_eq!(truncate_output(String::new()), "");
    }

    #[test]
    fn test_truncate_output_multibyte() {
        let emoji = "😀".repeat(MAX_NU_OUTPUT / 4 + 50);
        let result = truncate_output(emoji);
        assert!(result.ends_with("[output truncated]"));
        // Valid UTF-8 (would panic on from_utf8 check).
        String::from_utf8(result.into_bytes()).unwrap();
    }

    // -- format_tool_result dispatch --

    #[test]
    fn test_format_tool_result_unknown_passthrough() {
        assert_eq!(format_tool_result("NuShell", "raw text"), "raw text");
        assert_eq!(format_tool_result("unknown", "raw text"), "raw text");
    }

    // -- translate_tool_call missing-param errors --

    #[test]
    fn test_translate_write_missing_content() {
        let input = serde_json::json!({"file_path": "f.txt"});
        assert!(translate_tool_call("Write", &input).is_err());
    }

    #[test]
    fn test_translate_write_missing_path() {
        let input = serde_json::json!({"content": "hi"});
        assert!(translate_tool_call("Write", &input).is_err());
    }

    #[test]
    fn test_translate_edit_missing_old_string() {
        let input = serde_json::json!({"file_path": "f.txt", "new_string": "x"});
        assert!(translate_tool_call("Edit", &input).is_err());
    }

    #[test]
    fn test_translate_edit_missing_new_string() {
        let input = serde_json::json!({"file_path": "f.txt", "old_string": "x"});
        assert!(translate_tool_call("Edit", &input).is_err());
    }

    #[test]
    fn test_translate_glob_missing_pattern() {
        let input = serde_json::json!({});
        assert!(translate_tool_call("Glob", &input).is_err());
    }

    #[test]
    fn test_translate_grep_missing_pattern() {
        let input = serde_json::json!({});
        assert!(translate_tool_call("Grep", &input).is_err());
    }

    // -- nu_result_to_exec tests --

    #[test]
    fn test_nu_result_to_exec_ok() {
        let result = nu_result_to_exec(
            "tu_1".into(),
            Ok(NuOutput {
                content: "hello".into(),
                is_error: false,
            }),
        );
        assert_eq!(result.content, "hello");
        assert!(!result.is_error);
    }

    #[test]
    fn test_nu_result_to_exec_ok_error() {
        let result = nu_result_to_exec(
            "tu_1".into(),
            Ok(NuOutput {
                content: "err".into(),
                is_error: true,
            }),
        );
        assert_eq!(result.content, "err");
        assert!(result.is_error);
    }

    #[test]
    fn test_nu_result_to_exec_err() {
        let result = nu_result_to_exec("tu_1".into(), Err("failed".into()));
        assert_eq!(result.content, "failed");
        assert!(result.is_error);
    }

    // -- format_nu_output tests --

    #[test]
    fn test_format_nu_output_empty() {
        assert_eq!(format_nu_output(String::new()), "[no output]");
    }

    #[test]
    fn test_format_nu_output_normal() {
        assert_eq!(format_nu_output("hello world".to_owned()), "hello world");
    }

    #[test]
    fn test_format_nu_output_truncation() {
        let big = "x".repeat(MAX_NU_OUTPUT + 100);
        let formatted = format_nu_output(big);
        assert!(formatted.len() <= MAX_NU_OUTPUT + 20); // truncation marker
        assert!(formatted.ends_with("[output truncated]"));
    }

    #[test]
    fn test_format_nu_output_truncation_multibyte() {
        // U+1F600 (😀) is 4 bytes in UTF-8. Fill past the limit.
        let emoji = "😀".repeat(MAX_NU_OUTPUT / 4 + 50);
        let formatted = format_nu_output(emoji);
        assert!(formatted.ends_with("[output truncated]"));
        // Verify valid UTF-8 (would panic on invalid).
        let _ = formatted.as_bytes();
    }

    #[tokio::test]
    async fn test_nu_missing_command_param() {
        let result = exec("nu", serde_json::json!({}), ToolGrant::NU).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    /// Nonexistent project root — the nu session fails to spawn.
    #[tokio::test]
    async fn test_nu_bad_root_fails() {
        let gone = TempDir::new().unwrap().into_path();
        std::fs::remove_dir(&gone).unwrap();
        let result = exec_in(
            "nu",
            serde_json::json!({"command": "echo hello"}),
            &gone,
            ToolGrant::NU,
        )
        .await;
        assert!(
            result.is_error,
            "expected error for nonexistent root, got: {}",
            result.content,
        );
    }

    // -- read_file edge cases --

    #[tokio::test]
    async fn test_read_file_missing_path_param() {
        let result = exec("read_file", serde_json::json!({}), ToolGrant::READ).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn test_read_file_directory_rejected() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("subdir")).unwrap();
        let result = exec_in(
            "read_file",
            serde_json::json!({"path": "subdir"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not a file"));
    }

    // -- glob edge cases --

    #[tokio::test]
    async fn test_glob_no_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "").unwrap();
        let result = exec_in(
            "glob",
            serde_json::json!({"pattern": "*.rs"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.is_empty());
    }

    #[tokio::test]
    async fn test_glob_missing_pattern_param() {
        let result = exec("glob", serde_json::json!({}), ToolGrant::READ).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn test_glob_invalid_pattern() {
        let result = exec(
            "glob",
            serde_json::json!({"pattern": "[invalid"}),
            ToolGrant::READ,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid glob"));
    }

    // -- grep edge cases --

    #[tokio::test]
    async fn test_grep_no_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello world\n").unwrap();
        let result = exec_in(
            "grep",
            serde_json::json!({"pattern": "zzz_no_match"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.is_empty());
    }

    #[tokio::test]
    async fn test_grep_missing_pattern_param() {
        let result = exec("grep", serde_json::json!({}), ToolGrant::READ).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn test_grep_skips_binary_files() {
        let tmp = TempDir::new().unwrap();
        let mut binary_content = b"fn match_me\n".to_vec();
        binary_content.extend_from_slice(&[0u8; 100]);
        std::fs::write(tmp.path().join("bin.dat"), &binary_content).unwrap();
        std::fs::write(tmp.path().join("text.rs"), "fn match_me\n").unwrap();
        let result = exec_in(
            "grep",
            serde_json::json!({"pattern": "match_me"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("text.rs"));
        assert!(!result.content.contains("bin.dat"));
    }

    // -- edit_file edge cases --

    #[tokio::test]
    async fn test_edit_file_not_found() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello world").unwrap();
        let result = exec_in(
            "edit_file",
            serde_json::json!({"path": "f.txt", "old_string": "zzz", "new_string": "aaa"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn test_edit_file_missing_params() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello").unwrap();
        let result = exec_in(
            "edit_file",
            serde_json::json!({"path": "f.txt"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn test_edit_file_nonexistent_file() {
        let result = exec(
            "edit_file",
            serde_json::json!({"path": "nope.txt", "old_string": "a", "new_string": "b"}),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
    }

    // -- write_file edge cases --

    #[tokio::test]
    async fn test_write_file_missing_params() {
        let result = exec(
            "write_file",
            serde_json::json!({"path": "f.txt"}),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[test]
    fn test_format_nu_output_exact_limit() {
        let exact = "x".repeat(MAX_NU_OUTPUT);
        let formatted = format_nu_output(exact.clone());
        assert_eq!(formatted, exact);
        assert!(!formatted.contains("[output truncated]"));
    }

    #[tokio::test]
    async fn test_write_file_overwrites() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "old content").unwrap();
        let result = exec_in(
            "write_file",
            serde_json::json!({"path": "f.txt", "content": "new content"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(!result.is_error);
        let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
        assert_eq!(content, "new content");
    }
}
