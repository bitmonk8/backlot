// Tool access flags: READ, WRITE, BASH, TASK, WEB presets.

use bitflags::bitflags;
use globset::GlobBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_GLOB_RESULTS: usize = 1000;
const MAX_GREP_OUTPUT: usize = 64 * 1024;
const MAX_GREP_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_BASH_OUTPUT: usize = 64 * 1024;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
const MAX_BASH_TIMEOUT_SECS: u64 = 600;

bitflags! {
    /// Permission flags controlling which tools an agent call may use.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ToolGrant: u8 {
        const READ  = 0b0000_0001;
        const WRITE = 0b0000_0010;
        const BASH  = 0b0000_0100;
        const TASK  = 0b0000_1000;
        const WEB   = 0b0001_0000;
    }
}

/// Method categories that map to different tool grant sets.
#[derive(Debug, Clone, Copy)]
pub enum AgentMethod {
    /// Assessment / verification / checkpoint / recovery — read-only analysis.
    Analyze,
    /// Leaf execution — needs read, write, bash.
    Execute,
    /// Design and decompose — needs read for exploration.
    Decompose,
}

/// Returns the tool grant set appropriate for a given agent method.
pub fn phase_tools(method: AgentMethod) -> ToolGrant {
    match method {
        AgentMethod::Execute => ToolGrant::READ | ToolGrant::WRITE | ToolGrant::BASH,
        AgentMethod::Analyze | AgentMethod::Decompose => ToolGrant::READ,
    }
}

/// A tool definition suitable for inclusion in a Flick config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlickToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

/// Returns tool definitions for all tools permitted by the given grant.
pub fn tool_definitions(grant: ToolGrant) -> Vec<FlickToolDef> {
    let mut tools = Vec::new();

    if grant.contains(ToolGrant::READ) {
        tools.push(FlickToolDef {
            name: "read_file".into(),
            description: "Read the contents of a file at the given path.".into(),
            input_schema: serde_json::json!({
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
            input_schema: serde_json::json!({
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
            input_schema: serde_json::json!({
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
            input_schema: serde_json::json!({
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
            input_schema: serde_json::json!({
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

    if grant.contains(ToolGrant::BASH) {
        tools.push(FlickToolDef {
            name: "bash".into(),
            description: "Execute a bash command and return its output.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to execute" },
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
        return Err(format!(
            "path escapes project root: {}",
            resolved.display()
        ));
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
        "bash" => Some(ToolGrant::BASH),
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

    let result = match name {
        "read_file" => tool_read_file(input, project_root).await,
        "glob" => tool_glob(input, project_root).await,
        "grep" => tool_grep(input, project_root).await,
        "write_file" => tool_write_file(input, project_root).await,
        "edit_file" => tool_edit_file(input, project_root).await,
        "bash" => tool_bash(input, project_root).await,
        _ => Err(format!("unknown tool: {name}")),
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

    let re = Regex::new(&pattern).map_err(|e| format!("invalid regex: {e}"))?;

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
                if let Ok(rel) = path.strip_prefix(&search_dir) {
                    if !gf.is_match(&*rel.to_string_lossy()) {
                        continue;
                    }
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
        return Err(format!(
            "old_string not found in {}",
            path.display()
        ));
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

async fn tool_bash(input: &JsonValue, project_root: &Path) -> Result<String, String> {
    let command = get_str(input, "command")?;
    let timeout_secs = input
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS)
        .min(MAX_BASH_TIMEOUT_SECS);

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|e| format!("spawn error: {e}"))?;

    let timeout_dur = std::time::Duration::from_secs(timeout_secs);
    let output = tokio::time::timeout(timeout_dur, child.wait_with_output())
        .await
        .map_err(|_| format!("command timed out after {timeout_secs}s"))?
        .map_err(|e| format!("command failed: {e}"))?;

    let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // Truncate large outputs on char boundaries to avoid panic
    if stdout.len() > MAX_BASH_OUTPUT {
        let mut end = MAX_BASH_OUTPUT;
        while !stdout.is_char_boundary(end) {
            end -= 1;
        }
        stdout.truncate(end);
        stdout.push_str("\n[stdout truncated]");
    }
    if stderr.len() > MAX_BASH_OUTPUT {
        let mut end = MAX_BASH_OUTPUT;
        while !stderr.is_char_boundary(end) {
            end -= 1;
        }
        stderr.truncate(end);
        stderr.push_str("\n[stderr truncated]");
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let mut result = String::new();

    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(&stderr);
    }

    if exit_code != 0 {
        if !result.is_empty() {
            result.push('\n');
        }
        let _ = write!(result, "[exit code: {exit_code}]");
    }

    if result.is_empty() {
        result.push_str("[no output]");
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn analyze_gets_read_only() {
        let grant = phase_tools(AgentMethod::Analyze);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(!grant.contains(ToolGrant::BASH));
    }

    #[test]
    fn execute_gets_read_write_bash() {
        let grant = phase_tools(AgentMethod::Execute);
        assert!(grant.contains(ToolGrant::READ));
        assert!(grant.contains(ToolGrant::WRITE));
        assert!(grant.contains(ToolGrant::BASH));
    }

    #[test]
    fn read_only_tools() {
        let tools = tool_definitions(ToolGrant::READ);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"bash"));
    }

    #[test]
    fn full_tools() {
        let grant = ToolGrant::READ | ToolGrant::WRITE | ToolGrant::BASH;
        let tools = tool_definitions(grant);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"bash"));
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
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "write_file",
            &serde_json::json!({"path": "x.txt", "content": "hi"}),
            tmp.path(),
            ToolGrant::READ, // no WRITE
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("not permitted"));
    }

    #[tokio::test]
    async fn test_grant_check_unknown_tool() {
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "nonexistent_tool",
            &serde_json::json!({}),
            tmp.path(),
            ToolGrant::READ | ToolGrant::WRITE | ToolGrant::BASH,
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
        let result = execute_tool(
            "tu_1".into(),
            "read_file",
            &serde_json::json!({"path": "test.txt"}),
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
        let big = "x".repeat(MAX_READ_BYTES as usize + 100);
        std::fs::write(tmp.path().join("big.txt"), &big).unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "read_file",
            &serde_json::json!({"path": "big.txt"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("[truncated"));
    }

    #[tokio::test]
    async fn test_read_file_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "read_file",
            &serde_json::json!({"path": "nope.txt"}),
            tmp.path(),
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
        let result = execute_tool(
            "tu_1".into(),
            "glob",
            &serde_json::json!({"pattern": "**/*.rs"}),
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
        std::fs::write(tmp.path().join("code.rs"), "fn main() {\n    println!(\"hello\");\n}\n")
            .unwrap();
        std::fs::write(tmp.path().join("data.txt"), "no match here\n").unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "grep",
            &serde_json::json!({"pattern": "fn main"}),
            tmp.path(),
            ToolGrant::READ,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("code.rs"));
        assert!(result.content.contains("fn main"));
        assert!(!result.content.contains("data.txt"));
    }

    // -- write_file tests --

    #[tokio::test]
    async fn test_write_file_creates() {
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "write_file",
            &serde_json::json!({"path": "sub/new.txt", "content": "created"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("wrote"));
        let content = std::fs::read_to_string(tmp.path().join("sub/new.txt")).unwrap();
        assert_eq!(content, "created");
    }

    // -- edit_file tests --

    #[tokio::test]
    async fn test_edit_file_replaces() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello world").unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "edit_file",
            &serde_json::json!({"path": "f.txt", "old_string": "world", "new_string": "rust"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "edit_file",
            &serde_json::json!({"path": "f.txt", "old_string": "aaa", "new_string": "bbb"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("2 times"));
    }

    // -- bash tests --

    #[tokio::test]
    async fn test_bash_echo() {
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "bash",
            &serde_json::json!({"command": "echo hello"}),
            tmp.path(),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "bash",
            &serde_json::json!({"command": "sleep 10", "timeout": 2}),
            tmp.path(),
            ToolGrant::BASH,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let tmp = TempDir::new().unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "bash",
            &serde_json::json!({"command": "exit 42"}),
            tmp.path(),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error); // bash tool reports exit code in content, not as error
        assert!(result.content.contains("[exit code: 42]"));
    }
}
