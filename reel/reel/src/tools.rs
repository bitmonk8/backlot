// Tool grant flags, tool definitions, nu command translation, and execution dispatch.
//
// All file tools (Read, Write, Edit, Glob, Grep) execute through the nu
// session as `reel <verb>` custom commands. The `NuShell` tool provides direct
// nu command execution.

use crate::nu_session::{NuOutput, NuSession};
use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt::Write;
use std::path::Path;

/// Error returned when parsing tool grant names.
#[derive(Debug, Clone)]
pub struct GrantParseError {
    /// The unrecognized grant name.
    pub name: String,
}

impl std::fmt::Display for GrantParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown grant: {}", self.name)
    }
}

impl std::error::Error for GrantParseError {}

const MAX_NU_OUTPUT: usize = 64 * 1024;
const DEFAULT_NU_TIMEOUT_SECS: u64 = 120;
const MAX_NU_TIMEOUT_SECS: u64 = 600;

bitflags! {
    /// Permission flags controlling which tools an agent call may use.
    ///
    /// `TOOLS` enables the tool loop and read-only tools (Read, Glob, Grep,
    /// NuShell). `WRITE` adds mutation tools (Write, Edit) and sandbox write
    /// access — implies `TOOLS`. `NETWORK` enables outbound network in the
    /// sandbox — implies `TOOLS`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ToolGrant: u8 {
        const WRITE   = 0b0000_0001;
        const TOOLS   = 0b0000_0010;
        const NETWORK = 0b0000_0100;
    }
}

impl ToolGrant {
    /// Parse a list of grant names into a `ToolGrant` bitflag.
    ///
    /// `"write"` and `"network"` imply `TOOLS` — callers need not specify
    /// `"tools"` explicitly when requesting write or network access.
    pub fn from_names(names: &[impl AsRef<str>]) -> Result<Self, GrantParseError> {
        let mut flags = Self::empty();
        for name in names {
            match name.as_ref() {
                "write" => flags |= Self::WRITE | Self::TOOLS,
                "tools" => flags |= Self::TOOLS,
                "network" => flags |= Self::NETWORK | Self::TOOLS,
                other => {
                    return Err(GrantParseError {
                        name: other.to_string(),
                    });
                }
            }
        }
        Ok(flags)
    }

    /// Enforce invariants: WRITE implies TOOLS, NETWORK implies TOOLS.
    ///
    /// Call this on grants constructed directly via bitflags to ensure
    /// they behave identically to grants produced by `from_names()`.
    #[must_use]
    pub const fn normalize(self) -> Self {
        if self.contains(Self::WRITE) || self.contains(Self::NETWORK) {
            self.union(Self::TOOLS)
        } else {
            self
        }
    }

    /// Return the canonical grant names for the active flags.
    ///
    /// The order is deterministic: `["tools", "write", "network"]`.
    pub fn to_names(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.contains(Self::TOOLS) {
            names.push("tools");
        }
        if self.contains(Self::WRITE) {
            names.push("write");
        }
        if self.contains(Self::NETWORK) {
            names.push("network");
        }
        names
    }
}

/// A tool definition describing a tool's name, description, and JSON Schema parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
}

/// Returns tool definitions for all tools permitted by the given grant.
///
/// All file tools execute as nu custom commands (`reel read`, etc.).
/// `NuShell` provides direct nu command execution.
pub fn tool_definitions(grant: ToolGrant) -> Vec<ToolDefinition> {
    let grant = grant.normalize();
    let mut tools = Vec::new();

    // Read-only tools: available when TOOLS is granted
    if grant.contains(ToolGrant::TOOLS) {
        tools.push(ToolDefinition {
            name: "Read".into(),
            description: "Read the contents of a file. Returns lines with line numbers. For large files, use offset and limit to read specific sections.".into(),
            parameters: with_timeout(serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute or project-relative file path" },
                    "offset": { "type": "integer", "description": "Line number to start reading from (1-based). Omit to start from the beginning." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return. Omit to read up to the default cap." }
                },
                "required": ["file_path"]
            })),
        });
        tools.push(ToolDefinition {
            name: "Glob".into(),
            description: "Find files matching a glob pattern. Returns matching file paths sorted by modification time.".into(),
            parameters: with_timeout(serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs, src/**/*.ts)" },
                    "path": { "type": "string", "description": "Directory to search in. Defaults to project root." },
                    "depth": { "type": "integer", "description": "Max directory traversal depth. Default: 20." }
                },
                "required": ["pattern"]
            })),
        });
        tools.push(ToolDefinition {
            name: "Grep".into(),
            description: "Search file contents for a regex pattern. Powered by ripgrep.".into(),
            parameters: with_timeout(serde_json::json!({
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
            })),
        });
    }

    // Write tools: available when WRITE is granted (normalize ensures TOOLS is set)
    if grant.contains(ToolGrant::WRITE) {
        tools.push(ToolDefinition {
            name: "Write".into(),
            description: "Write content to a file, creating parent directories if necessary. Overwrites existing files.".into(),
            parameters: with_timeout(serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "File path to write to" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["file_path", "content"]
            })),
        });
        tools.push(ToolDefinition {
            name: "Edit".into(),
            description: "Replace an exact string match in a file. By default, old_string must appear exactly once (prevents ambiguous edits). Set replace_all to replace every occurrence.".into(),
            parameters: with_timeout(serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "File path to edit" },
                    "old_string": { "type": "string", "description": "Exact text to find and replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences instead of requiring uniqueness. Default: false." }
                },
                "required": ["file_path", "old_string", "new_string"]
            })),
        });
    }

    // NuShell tool: available when TOOLS is granted
    if grant.contains(ToolGrant::TOOLS) {
        tools.push(ToolDefinition {
            name: "NuShell".into(),
            description: "Execute a NuShell command or pipeline and return its output. Uses NuShell syntax (not POSIX sh). Session state (variables, env, cwd) persists across calls within the same task.".into(),
            parameters: with_timeout(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The NuShell command to execute" },
                    "description": { "type": "string", "description": "Brief description of what this command does" }
                },
                "required": ["command"]
            })),
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

/// Map tool name to the required grant flags.
fn required_grant(name: &str) -> Option<ToolGrant> {
    match name {
        "Write" | "Edit" => Some(ToolGrant::WRITE | ToolGrant::TOOLS),
        "NuShell" | "Read" | "Glob" | "Grep" => Some(ToolGrant::TOOLS),
        _ => None,
    }
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
/// `NuShell` is handled separately in `execute_tool` (direct
/// pass-through to `tool_nu`).
fn translate_tool_call(name: &str, input: &JsonValue) -> Result<String, String> {
    let cmd = match name {
        "Read" => translate_read(input),
        "Write" => translate_write(input),
        "Edit" => translate_edit(input),
        "Glob" => translate_glob(input),
        "Grep" => translate_grep(input),
        _ => Err(format!("unknown tool: {name}")),
    }?;
    Ok(format!("{cmd} | to json -r"))
}

fn translate_read(input: &JsonValue) -> Result<String, String> {
    let path = get_str(input, "file_path")?;
    let mut cmd = format!("reel read {}", quote_nu(path));
    if let Some(offset) = input.get("offset").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --offset {offset}");
    }
    if let Some(limit) = input.get("limit").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --limit {limit}");
    }
    Ok(cmd)
}

fn translate_write(input: &JsonValue) -> Result<String, String> {
    let path = get_str(input, "file_path")?;
    let content = get_str(input, "content")?;
    Ok(format!(
        "reel write {} {}",
        quote_nu(path),
        quote_nu(content)
    ))
}

fn translate_edit(input: &JsonValue) -> Result<String, String> {
    let path = get_str(input, "file_path")?;
    let old_string = get_str(input, "old_string")?;
    let new_string = get_str(input, "new_string")?;
    let mut cmd = format!(
        "reel edit {} {} {}",
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
    let mut cmd = format!("reel glob {}", quote_nu(pattern));
    if let Some(path) = get_str_opt(input, "path") {
        let _ = write!(cmd, " --path {}", quote_nu(path));
    }
    if let Some(depth) = input.get("depth").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --depth {depth}");
    }
    Ok(cmd)
}

fn translate_grep(input: &JsonValue) -> Result<String, String> {
    let pattern = get_str(input, "pattern")?;
    let mut cmd = format!("reel grep {}", quote_nu(pattern));
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
    if input.get("line_numbers").and_then(JsonValue::as_bool) == Some(false) {
        cmd.push_str(" --no-line-numbers");
    }
    if let Some(n) = input.get("context_after").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --context-after {n}");
    }
    if let Some(n) = input.get("context_before").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --context-before {n}");
    }
    if let Some(n) = input.get("context").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --context {n}");
    }
    if input
        .get("multiline")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        cmd.push_str(" --multiline");
    }
    if let Some(n) = input.get("head_limit").and_then(JsonValue::as_u64) {
        let _ = write!(cmd, " --head-limit {n}");
    }
    Ok(cmd)
}

/// Format structured JSON output from a nu command into Claude-friendly text.
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
        let _ = write!(
            output,
            "(0 lines returned, {total_lines} total, {size} bytes)"
        );
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

/// Add the shared timeout property to a JSON Schema `parameters` object.
///
/// Inserts the `"timeout"` key into `params["properties"]` and returns the
/// modified value for inline use in `serde_json::json!` builders.
fn with_timeout(mut params: JsonValue) -> JsonValue {
    if let Some(props) = params
        .get_mut("properties")
        .and_then(JsonValue::as_object_mut)
    {
        props.insert(
            "timeout".to_string(),
            serde_json::json!({
                "type": "integer",
                "description": "Timeout in seconds. Default: 120, min: 1, max: 600."
            }),
        );
    }
    params
}

/// Extract timeout from tool input, defaulting to `DEFAULT_NU_TIMEOUT_SECS` when absent,
/// clamped to `[1, MAX_NU_TIMEOUT_SECS]`.
fn parse_timeout(input: &JsonValue) -> u64 {
    input
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .unwrap_or(DEFAULT_NU_TIMEOUT_SECS)
        .clamp(1, MAX_NU_TIMEOUT_SECS)
}

/// Execute a tool call, checking grants and dispatching to the implementation.
///
/// All tools route through the nu session. File tools (Read, Write, Edit, Glob,
/// Grep) are translated into `reel <verb>` nu commands. `NuShell` is a direct
/// pass-through to `nu_session.evaluate()`.
pub async fn execute_tool(
    tool_use_id: String,
    name: &str,
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
    nu_session: &NuSession,
) -> ToolExecResult {
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

    // NuShell is a direct pass-through to nu_session.evaluate().
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

    // Allow model-provided timeout override for file tools (same logic as NuShell tool).
    let timeout_secs = parse_timeout(input);

    // Execute via nu session, then format+truncate successful output.
    // Empty output is valid (Glob/Grep with no matches), so don't replace it.
    let result = nu_session
        .evaluate(&nu_command, timeout_secs, project_root, grant)
        .await
        .map(|out| {
            if out.is_error {
                out
            } else {
                NuOutput {
                    content: truncate_output(format_tool_result(name, &out.content)),
                    is_error: false,
                    stderr: out.stderr,
                }
            }
        });

    nu_result_to_exec(tool_use_id, result)
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

async fn tool_nu(
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
    nu_session: &NuSession,
) -> Result<NuOutput, String> {
    let command = get_str(input, "command")?;
    let timeout_secs = parse_timeout(input);

    let mut result = nu_session
        .evaluate(command, timeout_secs, project_root, grant)
        .await?;

    result.content = format_nu_output(result.content);

    Ok(result)
}

/// Truncate output to `MAX_NU_OUTPUT` without replacing empty strings.
/// Used for tool results where empty output is semantically valid.
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
    fn read_only_tools() {
        let tools = tool_definitions(ToolGrant::TOOLS);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Glob"));
        assert!(names.contains(&"Grep"));
        assert!(names.contains(&"NuShell"));
        assert!(!names.contains(&"Write"));
        assert!(!names.contains(&"Edit"));
    }

    #[test]
    fn full_tools() {
        let grant = ToolGrant::WRITE | ToolGrant::TOOLS;
        let tools = tool_definitions(grant);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Write"));
        assert!(names.contains(&"Edit"));
        assert!(names.contains(&"Glob"));
        assert!(names.contains(&"Grep"));
        assert!(names.contains(&"NuShell"));
    }

    #[test]
    fn empty_grant_no_tools() {
        let tools = tool_definitions(ToolGrant::empty());
        assert!(tools.is_empty());
    }

    // -- grant check tests --

    #[tokio::test]
    async fn test_grant_check_denies() {
        let result = exec(
            "Write",
            serde_json::json!({"file_path": "x.txt", "content": "hi"}),
            ToolGrant::TOOLS, // no WRITE
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
            ToolGrant::WRITE | ToolGrant::TOOLS,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown tool"));
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

    // -- quote_nu adversarial input tests --

    #[test]
    fn test_quote_nu_subshell_expression() {
        // $(rm -rf /) must not be interpreted as a subshell — single-quoted strings
        // in nu treat $ as literal.
        assert_eq!(quote_nu("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn test_quote_nu_subshell_with_single_quote() {
        // Combine subshell syntax with single quote to force raw string path.
        // The $ must remain inert inside the raw string.
        let result = quote_nu("$(echo 'pwned')");
        assert_eq!(result, r"r#'$(echo 'pwned')'#");
    }

    #[test]
    fn test_quote_nu_null_byte() {
        // Null bytes in input — quote_nu must not panic or produce invalid output.
        // Nu single-quoted strings can contain \0 as a literal byte.
        let result = quote_nu("before\0after");
        assert_eq!(result, "'before\0after'");
    }

    #[test]
    fn test_quote_nu_null_byte_with_single_quote() {
        // Null byte + single quote forces raw string path.
        let result = quote_nu("it's\0here");
        assert_eq!(result, "r#'it's\0here'#");
    }

    #[test]
    fn test_quote_nu_multiline_with_closing_delimiter() {
        // Multi-line string that contains the '# closing delimiter on a separate line.
        // Must escalate to r##'...'## to avoid premature close.
        let input = "line1\n'#\nline3";
        let result = quote_nu(input);
        assert_eq!(result, "r##'line1\n'#\nline3'##");
    }

    #[test]
    fn test_quote_nu_multiline_with_escalating_delimiters() {
        // Contains both '# and '## — must escalate to r###'...'###.
        let input = "a'#b\n'##c";
        let result = quote_nu(input);
        assert_eq!(result, "r###'a'#b\n'##c'###");
    }

    #[test]
    fn test_quote_nu_all_single_quotes() {
        let result = quote_nu("'''");
        assert_eq!(result, "r#'''''#");
    }

    #[test]
    fn test_quote_nu_semicolon_command_separator() {
        // Semicolons separate commands in nu — must be inert inside quotes.
        assert_eq!(quote_nu("a; rm -rf /"), "'a; rm -rf /'");
    }

    #[test]
    fn test_quote_nu_pipe_operator() {
        // Pipe operator must be inert inside quotes.
        assert_eq!(quote_nu("x | malicious"), "'x | malicious'");
    }

    #[test]
    fn test_quote_nu_glob_characters() {
        // Glob wildcards — must be literal inside quotes.
        assert_eq!(quote_nu("*?[abc]"), "'*?[abc]'");
    }

    #[test]
    fn test_quote_nu_newline_with_closing_single_quote() {
        // Multi-line where a line ends with ' — still safe in simple single quotes
        // because nu raw strings only close on '# sequences.
        assert_eq!(quote_nu("line1'\nline2"), "r#'line1'\nline2'#");
    }

    #[test]
    fn test_quote_nu_crlf_line_endings() {
        assert_eq!(quote_nu("a\r\nb"), "'a\r\nb'");
    }

    #[test]
    fn test_quote_nu_tab_and_control_chars() {
        assert_eq!(quote_nu("a\tb\x07c"), "'a\tb\x07c'");
    }

    // -- translate_tool_call tests --

    #[test]
    fn test_translate_read_basic() {
        let input = serde_json::json!({"file_path": "src/main.rs"});
        let cmd = translate_tool_call("Read", &input).unwrap();
        assert_eq!(cmd, "reel read 'src/main.rs' | to json -r");
    }

    #[test]
    fn test_translate_read_with_offset_limit() {
        let input = serde_json::json!({"file_path": "f.txt", "offset": 10, "limit": 50});
        let cmd = translate_tool_call("Read", &input).unwrap();
        assert_eq!(cmd, "reel read 'f.txt' --offset 10 --limit 50 | to json -r");
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
        assert_eq!(cmd, "reel write 'out.txt' 'hello' | to json -r");
    }

    #[test]
    fn test_translate_write_special_chars() {
        let input = serde_json::json!({"file_path": "f.txt", "content": "it's a \"test\""});
        let cmd = translate_tool_call("Write", &input).unwrap();
        assert!(cmd.starts_with("reel write 'f.txt' r#'it's a \"test\"'#"));
    }

    #[test]
    fn test_translate_edit_basic() {
        let input = serde_json::json!({
            "file_path": "f.txt",
            "old_string": "old",
            "new_string": "new"
        });
        let cmd = translate_tool_call("Edit", &input).unwrap();
        assert_eq!(cmd, "reel edit 'f.txt' 'old' 'new' | to json -r");
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
            "reel edit 'f.txt' 'old' 'new' --replace-all | to json -r"
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
        assert_eq!(cmd, "reel edit 'f.txt' 'old' 'new' | to json -r");
    }

    #[test]
    fn test_translate_glob_basic() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        let cmd = translate_tool_call("Glob", &input).unwrap();
        assert_eq!(cmd, "reel glob '**/*.rs' | to json -r");
    }

    #[test]
    fn test_translate_glob_with_path() {
        let input = serde_json::json!({"pattern": "*.txt", "path": "src"});
        let cmd = translate_tool_call("Glob", &input).unwrap();
        assert_eq!(cmd, "reel glob '*.txt' --path 'src' | to json -r");
    }

    #[test]
    fn test_translate_glob_with_depth() {
        let input = serde_json::json!({"pattern": "**/*.rs", "depth": 5});
        let cmd = translate_tool_call("Glob", &input).unwrap();
        assert_eq!(cmd, "reel glob '**/*.rs' --depth 5 | to json -r");
    }

    #[test]
    fn test_translate_glob_with_path_and_depth() {
        let input = serde_json::json!({"pattern": "*.txt", "path": "src", "depth": 10});
        let cmd = translate_tool_call("Glob", &input).unwrap();
        assert_eq!(
            cmd,
            "reel glob '*.txt' --path 'src' --depth 10 | to json -r"
        );
    }

    #[test]
    fn test_translate_grep_basic() {
        let input = serde_json::json!({"pattern": "fn main"});
        let cmd = translate_tool_call("Grep", &input).unwrap();
        assert_eq!(cmd, "reel grep 'fn main' | to json -r");
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
    fn test_translate_grep_line_numbers_true_omitted() {
        // line_numbers: true is the default — nu adds -n for content mode automatically.
        // No --line-numbers or --no-line-numbers flag should be emitted.
        let input = serde_json::json!({"pattern": "x", "line_numbers": true});
        let cmd = translate_tool_call("Grep", &input).unwrap();
        assert!(!cmd.contains("--line-numbers"));
        assert!(!cmd.contains("--no-line-numbers"));
    }

    #[test]
    fn test_translate_nushell_not_handled() {
        // NuShell is handled directly in execute_tool, not translate_tool_call
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

    // -- required_grant tests --

    #[test]
    fn test_required_grant_names() {
        assert_eq!(required_grant("Read"), Some(ToolGrant::TOOLS));
        assert_eq!(required_grant("Glob"), Some(ToolGrant::TOOLS));
        assert_eq!(required_grant("Grep"), Some(ToolGrant::TOOLS));
        assert_eq!(
            required_grant("Write"),
            Some(ToolGrant::WRITE | ToolGrant::TOOLS)
        );
        assert_eq!(
            required_grant("Edit"),
            Some(ToolGrant::WRITE | ToolGrant::TOOLS)
        );
        assert_eq!(required_grant("NuShell"), Some(ToolGrant::TOOLS));
        assert_eq!(required_grant("unknown"), None);
    }

    // -- execute_tool grant check --

    #[tokio::test]
    async fn test_write_denied_without_grant() {
        let result = exec(
            "Write",
            serde_json::json!({"file_path": "x.txt", "content": "hi"}),
            ToolGrant::TOOLS, // TOOLS but no WRITE
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not permitted"));
    }

    #[tokio::test]
    async fn test_read_denied_with_empty_grant() {
        let result = exec(
            "Read",
            serde_json::json!({"file_path": "x"}),
            ToolGrant::empty(),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not permitted"));
    }

    #[tokio::test]
    async fn test_read_denied_with_write_only() {
        let result = exec(
            "Read",
            serde_json::json!({"file_path": "x"}),
            ToolGrant::WRITE,
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
                stderr: None,
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
                stderr: None,
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
    async fn test_nushell_missing_command_param() {
        let result = exec("NuShell", serde_json::json!({}), ToolGrant::TOOLS).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    /// Nonexistent project root — the nu session fails to spawn.
    #[tokio::test]
    async fn test_nushell_bad_root_fails() {
        let gone = TempDir::new().unwrap().keep();
        std::fs::remove_dir(&gone).unwrap();
        let result = exec_in(
            "NuShell",
            serde_json::json!({"command": "echo hello"}),
            &gone,
            ToolGrant::TOOLS,
        )
        .await;
        assert!(
            result.is_error,
            "expected error for nonexistent root, got: {}",
            result.content,
        );
    }

    #[test]
    fn test_format_nu_output_exact_limit() {
        let exact = "x".repeat(MAX_NU_OUTPUT);
        let formatted = format_nu_output(exact.clone());
        assert_eq!(formatted, exact);
        assert!(!formatted.contains("[output truncated]"));
    }

    #[test]
    fn bare_write_produces_tool_definitions() {
        // After normalization, bare WRITE implies TOOLS, so tool definitions
        // include Write/Edit tools plus the read-only tools.
        let tools = tool_definitions(ToolGrant::WRITE);
        assert!(!tools.is_empty());
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Write"));
        assert!(names.contains(&"Edit"));
        assert!(names.contains(&"Read"));
    }

    // -- ToolGrant::from_names tests (issue #36) --

    #[test]
    fn from_names_empty_returns_empty_grant() {
        let empty: &[&str] = &[];
        let grant = ToolGrant::from_names(empty).unwrap();
        assert_eq!(grant, ToolGrant::empty());
    }

    #[test]
    fn from_names_write_implies_tools() {
        let grant = ToolGrant::from_names(&["write"]).unwrap();
        assert_eq!(grant, ToolGrant::WRITE | ToolGrant::TOOLS);
    }

    #[test]
    fn from_names_tools_only() {
        let grant = ToolGrant::from_names(&["tools"]).unwrap();
        assert_eq!(grant, ToolGrant::TOOLS);
    }

    #[test]
    fn from_names_network_implies_tools() {
        let grant = ToolGrant::from_names(&["network"]).unwrap();
        assert_eq!(grant, ToolGrant::NETWORK | ToolGrant::TOOLS);
    }

    #[test]
    fn from_names_combined_flags() {
        let grant = ToolGrant::from_names(&["write", "tools", "network"]).unwrap();
        assert_eq!(
            grant,
            ToolGrant::WRITE | ToolGrant::TOOLS | ToolGrant::NETWORK
        );
    }

    #[test]
    fn from_names_duplicate_flags_idempotent() {
        let grant = ToolGrant::from_names(&["write", "write", "tools"]).unwrap();
        assert_eq!(grant, ToolGrant::WRITE | ToolGrant::TOOLS);
    }

    #[test]
    fn from_names_write_and_network_imply_tools() {
        let grant = ToolGrant::from_names(&["write", "network"]).unwrap();
        assert_eq!(
            grant,
            ToolGrant::WRITE | ToolGrant::NETWORK | ToolGrant::TOOLS
        );
    }

    #[test]
    fn from_names_old_nu_name_rejected() {
        let err = ToolGrant::from_names(&["nu"]).unwrap_err();
        assert_eq!(err.name, "nu");
    }

    #[test]
    fn from_names_unknown_grant_error() {
        let err = ToolGrant::from_names(&["write", "bogus"]).unwrap_err();
        assert_eq!(err.name, "bogus");
        assert!(err.to_string().contains("unknown grant: bogus"));
    }

    #[test]
    fn from_names_unknown_grant_first_position() {
        let err = ToolGrant::from_names(&["invalid"]).unwrap_err();
        assert_eq!(err.name, "invalid");
    }

    #[test]
    fn from_names_case_sensitive() {
        // "Write" (capital W) is not recognized — only lowercase "write".
        let err = ToolGrant::from_names(&["Write"]).unwrap_err();
        assert_eq!(err.name, "Write");
    }

    #[test]
    fn from_names_with_string_vec() {
        // Verify it works with Vec<String> (not just &[&str]).
        let names = vec!["tools".to_string(), "write".to_string()];
        let grant = ToolGrant::from_names(&names).unwrap();
        assert_eq!(grant, ToolGrant::TOOLS | ToolGrant::WRITE);
    }

    #[test]
    fn from_names_empty_string_rejected() {
        let err = ToolGrant::from_names(&["write", "", "tools"]).unwrap_err();
        assert_eq!(err.name, "");
        assert!(err.to_string().contains("unknown grant: "));
    }

    // -- ToolGrant::to_names tests --

    #[test]
    fn to_names_empty() {
        assert!(ToolGrant::empty().to_names().is_empty());
    }

    #[test]
    fn to_names_all_flags() {
        let names = (ToolGrant::TOOLS | ToolGrant::WRITE | ToolGrant::NETWORK).to_names();
        assert_eq!(names, vec!["tools", "write", "network"]);
    }

    #[test]
    fn to_names_tools_only() {
        let names = ToolGrant::TOOLS.to_names();
        assert_eq!(names, vec!["tools"]);
    }

    #[test]
    fn grant_parse_error_is_std_error() {
        let err = GrantParseError { name: "foo".into() };
        let _: &dyn std::error::Error = &err;
    }

    // -- ToolGrant::normalize tests (issue #52) --

    #[test]
    fn normalize_write_implies_tools() {
        assert_eq!(
            ToolGrant::WRITE.normalize(),
            ToolGrant::WRITE | ToolGrant::TOOLS
        );
    }

    #[test]
    fn normalize_network_implies_tools() {
        assert_eq!(
            ToolGrant::NETWORK.normalize(),
            ToolGrant::NETWORK | ToolGrant::TOOLS
        );
    }

    #[test]
    fn normalize_empty_stays_empty() {
        assert_eq!(ToolGrant::empty().normalize(), ToolGrant::empty());
    }

    #[test]
    fn normalize_idempotent() {
        let grant = ToolGrant::WRITE | ToolGrant::NETWORK | ToolGrant::TOOLS;
        assert_eq!(grant.normalize(), grant);
    }

    // -- ToolGrant::to_names guard (issue #69) --

    #[test]
    fn to_names_covers_all_flags() {
        // If a new ToolGrant flag is added but to_names() is not updated,
        // this test will fail.
        assert_eq!(
            ToolGrant::all().to_names().len(),
            ToolGrant::all().bits().count_ones() as usize,
            "to_names() must cover all ToolGrant flags"
        );
    }

    // -- parse_timeout tests --

    #[test]
    fn test_parse_timeout_default_when_absent() {
        let input = serde_json::json!({});
        assert_eq!(parse_timeout(&input), DEFAULT_NU_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_valid_value() {
        let input = serde_json::json!({"timeout": 30});
        assert_eq!(parse_timeout(&input), 30);
    }

    #[test]
    fn test_parse_timeout_clamped_to_max() {
        let input = serde_json::json!({"timeout": 9999});
        assert_eq!(parse_timeout(&input), MAX_NU_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_zero() {
        let input = serde_json::json!({"timeout": 0});
        assert_eq!(parse_timeout(&input), 1);
    }

    #[test]
    fn test_parse_timeout_non_integer_falls_back() {
        let input = serde_json::json!({"timeout": "fast"});
        assert_eq!(parse_timeout(&input), DEFAULT_NU_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_exact_max() {
        let input = serde_json::json!({"timeout": MAX_NU_TIMEOUT_SECS});
        assert_eq!(parse_timeout(&input), MAX_NU_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_one_over_max() {
        let input = serde_json::json!({"timeout": MAX_NU_TIMEOUT_SECS + 1});
        assert_eq!(parse_timeout(&input), MAX_NU_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_negative_falls_back() {
        let input = serde_json::json!({"timeout": -5});
        assert_eq!(parse_timeout(&input), DEFAULT_NU_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_exact_min() {
        let input = serde_json::json!({"timeout": 1});
        assert_eq!(parse_timeout(&input), 1);
    }

    #[test]
    fn test_parse_timeout_float_falls_back() {
        let input = serde_json::json!({"timeout": 3.5});
        assert_eq!(parse_timeout(&input), DEFAULT_NU_TIMEOUT_SECS);
    }

    // -- with_timeout tests --

    #[test]
    fn test_with_timeout_adds_property() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "foo": { "type": "string" }
            }
        });
        let result = with_timeout(params);
        let props = result["properties"].as_object().unwrap();
        assert!(props.contains_key("timeout"));
        assert_eq!(props["timeout"]["type"], "integer");
    }

    #[test]
    fn test_with_timeout_preserves_existing_properties() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "integer" }
            }
        });
        let result = with_timeout(params);
        let props = result["properties"].as_object().unwrap();
        assert!(props.contains_key("a"));
        assert!(props.contains_key("b"));
        assert!(props.contains_key("timeout"));
    }

    #[test]
    fn test_with_timeout_no_properties_key() {
        // No "properties" key — returns unchanged
        let params = serde_json::json!({"type": "object"});
        let result = with_timeout(params.clone());
        assert_eq!(result, params);
    }

    #[test]
    fn test_with_timeout_description_field() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "foo": { "type": "string" }
            }
        });
        let result = with_timeout(params);
        let desc = result["properties"]["timeout"]["description"]
            .as_str()
            .expect("timeout should have a description");
        assert!(desc.contains("120"), "description should mention default");
        assert!(desc.contains("600"), "description should mention max");
    }

    #[test]
    fn test_with_timeout_non_object_properties() {
        // "properties" is a string, not an object — returns unchanged
        let params = serde_json::json!({"type": "object", "properties": "not_an_object"});
        let result = with_timeout(params.clone());
        assert_eq!(result, params);
    }

    #[test]
    fn test_all_tool_definitions_have_timeout() {
        let grant = ToolGrant::WRITE | ToolGrant::TOOLS | ToolGrant::NETWORK;
        for tool in tool_definitions(grant) {
            let props = tool.parameters["properties"]
                .as_object()
                .unwrap_or_else(|| {
                    panic!("{} missing properties", tool.name);
                });
            assert!(
                props.contains_key("timeout"),
                "{} tool definition missing timeout property",
                tool.name,
            );
            let timeout = &props["timeout"];
            assert_eq!(
                timeout["type"], "integer",
                "{} timeout should be integer type",
                tool.name,
            );
            assert!(
                timeout
                    .get("description")
                    .and_then(|v| v.as_str())
                    .is_some(),
                "{} timeout should have a description string",
                tool.name,
            );
        }
    }

    #[tokio::test]
    async fn test_legacy_tool_names_rejected() {
        for name in ["read_file", "write_file", "edit_file", "glob", "grep", "nu"] {
            let result = exec(
                name,
                serde_json::json!({}),
                ToolGrant::WRITE | ToolGrant::TOOLS,
            )
            .await;
            assert!(result.is_error, "{name} should be rejected");
            assert!(
                result.content.contains("unknown tool"),
                "{name} should be unknown, got: {}",
                result.content,
            );
        }
    }
}
