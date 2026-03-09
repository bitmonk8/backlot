// Tool access flags: READ, WRITE, NU, TASK, WEB presets.

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
        const TASK  = 0b0000_1000;
        const WEB   = 0b0001_0000;
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
pub fn phase_tools(method: AgentMethod) -> ToolGrant {
    match method {
        AgentMethod::Execute => ToolGrant::READ | ToolGrant::WRITE | ToolGrant::NU,
        AgentMethod::Analyze => ToolGrant::READ,
        AgentMethod::Decompose => ToolGrant::READ | ToolGrant::NU,
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
pub fn tool_definitions(grant: ToolGrant) -> Vec<FlickToolDef> {
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

/// Map tool name to the required grant flag.
fn required_grant(name: &str) -> Option<ToolGrant> {
    match name {
        "read_file" | "glob" | "grep" => Some(ToolGrant::READ),
        "write_file" | "edit_file" => Some(ToolGrant::WRITE),
        "nu" => Some(ToolGrant::NU),
        _ => None,
    }
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

    // Nu tool returns a richer type because non-zero exit is not a dispatch
    // failure but must still set is_error for callers.
    if name == "nu" {
        return match tool_nu(input, project_root, grant, nu_session).await {
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
        };
    }

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

fn format_nu_output(raw: String) -> String {
    if raw.is_empty() {
        return "[no output]".into();
    }

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
    fn analyze_gets_read_only() {
        let grant = phase_tools(AgentMethod::Analyze);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(!grant.contains(ToolGrant::NU));
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
        let tools = tool_definitions(ToolGrant::READ);
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
        let tools = tool_definitions(grant);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"nu"));
    }

    #[test]
    fn empty_grant_no_tools() {
        let tools = tool_definitions(ToolGrant::empty());
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

    // -- nu tool tests --
    //
    // Integration tests for the nu tool require a `nu` binary (downloaded by
    // build.rs) and lot sandbox — not yet written (TODO).
    // Unit tests for format_nu_output are always enabled.

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
