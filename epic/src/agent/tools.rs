// Tool access flags: READ, WRITE, BASH, TASK, WEB presets.

use bitflags::bitflags;
use globset::GlobBuilder;
use lot::{SandboxCommand, SandboxPolicyBuilder, SandboxStdio};
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
        AgentMethod::Analyze => ToolGrant::READ,
        AgentMethod::Decompose => ToolGrant::READ | ToolGrant::BASH,
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

    if grant.contains(ToolGrant::BASH) {
        tools.push(FlickToolDef {
            name: "bash".into(),
            description: "Execute a bash command and return its output.".into(),
            parameters: serde_json::json!({
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

    // Bash returns a richer type because non-zero exit is not a dispatch
    // failure but must still set is_error for callers.
    if name == "bash" {
        return match tool_bash(input, project_root, grant).await {
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

/// Carries both content and error flag from bash execution, since a non-zero
/// exit is not a dispatch failure but should still surface via `is_error`.
struct BashOutput {
    content: String,
    is_error: bool,
}

/// Build a sandbox policy for a bash invocation based on the current tool grant.
///
/// - `WRITE` grant present → `project_root` is writable (Execute/Fix phases)
/// - `WRITE` grant absent  → `project_root` is read-only (Decompose/Verify phases)
fn build_sandbox_policy(project_root: &Path, grant: ToolGrant) -> lot::Result<lot::SandboxPolicy> {
    let mut builder = SandboxPolicyBuilder::new()
        .include_temp_dirs()
        .include_platform_exec_paths()
        .include_platform_lib_paths()
        .allow_network(true);

    if grant.contains(ToolGrant::WRITE) {
        builder = builder.write_path(project_root);
    } else {
        builder = builder.read_path(project_root);
    }

    builder.build()
}

async fn tool_bash(
    input: &JsonValue,
    project_root: &Path,
    grant: ToolGrant,
) -> Result<BashOutput, String> {
    let command = get_str(input, "command")?.to_owned();
    let timeout_secs = input
        .get("timeout")
        .and_then(JsonValue::as_u64)
        .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS)
        .min(MAX_BASH_TIMEOUT_SECS);

    let policy = match build_sandbox_policy(project_root, grant) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[epic] sandbox policy error ({e}), running unsandboxed");
            return tool_bash_unsandboxed(&command, project_root, timeout_secs).await;
        }
    };

    let project_root = project_root.to_path_buf();
    match tool_bash_sandboxed(command.clone(), project_root.clone(), policy, timeout_secs).await {
        Ok(output) => Ok(output),
        Err(SandboxSpawnError::SetupFailed(msg)) => {
            eprintln!("[epic] sandbox unavailable ({msg}), running unsandboxed");
            tool_bash_unsandboxed(&command, &project_root, timeout_secs).await
        }
        Err(SandboxSpawnError::Other(msg)) => Err(msg),
    }
}

enum SandboxSpawnError {
    /// Sandbox setup failed (permissions, unsupported platform) — caller
    /// should fall back to unsandboxed execution.
    SetupFailed(String),
    /// Non-recoverable error (timeout, command failure).
    Other(String),
}

async fn tool_bash_sandboxed(
    command: String,
    project_root: PathBuf,
    policy: lot::SandboxPolicy,
    timeout_secs: u64,
) -> Result<BashOutput, SandboxSpawnError> {
    let spawn_result = tokio::task::spawn_blocking(move || {
        let mut cmd = SandboxCommand::new("sh");
        cmd.args(["-c", &command]);
        cmd.cwd(&project_root);
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);
        cmd.stdin(SandboxStdio::Null);
        cmd.forward_common_env();

        lot::spawn(&policy, &cmd)
    })
    .await
    .map_err(|e| SandboxSpawnError::Other(format!("spawn task panicked: {e}")))?;

    let child = match spawn_result {
        Ok(c) => c,
        Err(e) => return Err(classify_spawn_error(e)),
    };

    let timeout = std::time::Duration::from_secs(timeout_secs);
    match child.wait_with_output_timeout(timeout).await {
        Ok(output) => Ok(bash_output_from(&output)),
        Err(lot::SandboxError::Timeout(_)) => {
            Err(format!("command timed out after {timeout_secs}s"))
        }
        Err(e) => Err(format!("command failed: {e}")),
    }
    .map_err(SandboxSpawnError::Other)
}

/// Classify a `lot::SandboxError` as either a setup failure (fallback to
/// unsandboxed) or a hard error (propagate).
fn classify_spawn_error(e: lot::SandboxError) -> SandboxSpawnError {
    match e {
        lot::SandboxError::Unsupported(msg)
        | lot::SandboxError::Setup(msg)
        | lot::SandboxError::InvalidPolicy(msg) => SandboxSpawnError::SetupFailed(msg),
        lot::SandboxError::Io(ref io) => match io.kind() {
            std::io::ErrorKind::PermissionDenied => SandboxSpawnError::SetupFailed(e.to_string()),
            _ => SandboxSpawnError::Other(e.to_string()),
        },
        lot::SandboxError::Cleanup(msg) => SandboxSpawnError::Other(msg),
        lot::SandboxError::Timeout(_) => SandboxSpawnError::Other(e.to_string()),
        // #[non_exhaustive]: unknown future variants → treat as setup
        // failure so we degrade gracefully rather than hard-fail.
        _ => SandboxSpawnError::SetupFailed(e.to_string()),
    }
}

/// Environment variables forwarded in the unsandboxed fallback path.
/// Mirrors the set used by `SandboxCommand::forward_common_env()`.
const UNSANDBOXED_ENV_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "SHELL",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SYSTEMROOT",
    "COMSPEC",
    "WINDIR",
    "PROGRAMFILES",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
];

/// Unsandboxed fallback — used when sandbox setup fails.
async fn tool_bash_unsandboxed(
    command: &str,
    project_root: &Path,
    timeout_secs: u64,
) -> Result<BashOutput, String> {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .env_clear();

    // Put child in its own process group so we can kill the whole tree on timeout.
    #[cfg(unix)]
    {
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid() is async-signal-safe. pre_exec runs between fork and exec.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    // Forward the same env vars lot's forward_common_env() uses.
    for key in UNSANDBOXED_ENV_KEYS {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    let child = cmd.spawn().map_err(|e| format!("spawn error: {e}"))?;
    let child_pid = child.id();

    let timeout_dur = std::time::Duration::from_secs(timeout_secs);
    if let Ok(wait_result) = tokio::time::timeout(timeout_dur, child.wait_with_output()).await {
        let output = wait_result.map_err(|e| format!("command failed: {e}"))?;
        Ok(bash_output_from(&output))
    } else {
        if let Some(pid) = child_pid {
            kill_process_tree(pid);
        }
        Err(format!("command timed out after {timeout_secs}s"))
    }
}

/// Kill the entire process group (Unix) or process tree (Windows) rooted
/// at the given PID. Used only for the unsandboxed fallback path.
#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    let Some(pid) = i32::try_from(pid).ok() else {
        return;
    };
    // SAFETY: negative pid targets the process group. The pid came from
    // a child we spawned into its own session via setsid().
    #[allow(unsafe_code)]
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn bash_output_from(output: &std::process::Output) -> BashOutput {
    BashOutput {
        content: format_bash_output(output),
        is_error: output.status.code() != Some(0),
    }
}

fn format_bash_output(output: &std::process::Output) -> String {
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

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: execute a tool in a fresh temp dir (for tests that don't need
    /// pre-populated files).
    async fn exec(name: &str, input: serde_json::Value, grant: ToolGrant) -> ToolExecResult {
        let tmp = TempDir::new().unwrap();
        execute_tool("tu_1".into(), name, &input, tmp.path(), grant).await
    }

    #[test]
    fn analyze_gets_read_only() {
        let grant = phase_tools(AgentMethod::Analyze);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(!grant.contains(ToolGrant::BASH));
    }

    #[test]
    fn decompose_gets_read_bash() {
        let grant = phase_tools(AgentMethod::Decompose);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(grant.contains(ToolGrant::BASH));
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
        let big = "x".repeat(usize::try_from(MAX_READ_BYTES).unwrap() + 100);
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
        std::fs::write(
            tmp.path().join("code.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
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

    #[tokio::test]
    async fn test_grep_regex_complexity_rejected() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "aaa\n").unwrap();
        let huge = format!("({}){{{}}}", "a|b|c|d|e|f|g|h|i|j".repeat(50), 9999);
        let result = execute_tool(
            "tu_1".into(),
            "grep",
            &serde_json::json!({"pattern": huge}),
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
        let result = execute_tool(
            "tu_1".into(),
            "grep",
            &serde_json::json!({"pattern": r"fn\s+\w+"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "grep",
            &serde_json::json!({"pattern": "fn hello", "glob": "*.rs"}),
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
        let result = exec(
            "bash",
            serde_json::json!({"command": "echo hello"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "sleep 10", "timeout": 2}),
            ToolGrant::BASH,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn test_bash_zero_exit_not_error() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "true"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_bash_nonzero_exit_is_error() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "exit 1"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("[exit code: 1]"));
    }

    /// Verify that on timeout, the sandboxed child and its descendants are killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_bash_timeout_kills_sandboxed_child() {
        let tmp = TempDir::new().unwrap();
        let marker = tmp.path().join("bg_alive");

        // Spawn a command that starts a background child writing a marker file.
        // On timeout, both sh and the background loop must die.
        let cmd = format!(
            "(while true; do touch {}; sleep 0.2; done) & sleep 60",
            marker.display()
        );

        let result = execute_tool(
            "tu_1".into(),
            "bash",
            &serde_json::json!({"command": cmd, "timeout": 2}),
            tmp.path(),
            ToolGrant::BASH | ToolGrant::WRITE,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));

        // Remove the marker and wait briefly; if the bg process survived
        // it will recreate the file.
        let _ = std::fs::remove_file(&marker);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(
            !marker.exists(),
            "background process survived timeout — process tree kill failed"
        );
    }

    /// Verify that calling `kill_process_tree` with a stale PID does not panic.
    #[cfg(unix)]
    #[test]
    fn test_kill_process_tree_stale_pid_unix() {
        // Use a PID that fits i32 but almost certainly does not exist.
        kill_process_tree(99_999_999);
    }

    /// Verify that calling `kill_process_tree` with a stale PID does not panic.
    #[cfg(windows)]
    #[test]
    fn test_kill_process_tree_stale_pid_windows() {
        kill_process_tree(u32::MAX - 1);
    }

    #[test]
    fn test_build_sandbox_policy_write_grant() {
        let tmp = TempDir::new().unwrap();
        let policy = build_sandbox_policy(tmp.path(), ToolGrant::WRITE | ToolGrant::BASH).unwrap();
        let canon = tmp.path().canonicalize().unwrap();

        // Project root is either directly in write_paths or covered by an
        // ancestor (e.g. on Windows where TempDir is inside %TEMP%).
        let covered_by_write = policy
            .write_paths
            .iter()
            .any(|w| canon.starts_with(w) || w.starts_with(&canon));
        assert!(
            covered_by_write,
            "project root should be writable (directly or via ancestor) when WRITE granted"
        );
        assert!(
            !policy.read_paths.contains(&canon),
            "project root should NOT be in read_paths when WRITE granted"
        );
    }

    #[test]
    fn test_build_sandbox_policy_no_write_grant() {
        let tmp = TempDir::new().unwrap();
        let policy = build_sandbox_policy(tmp.path(), ToolGrant::BASH).unwrap();
        let canon = tmp.path().canonicalize().unwrap();

        // Project root is in read_paths unless it overlaps with a write path
        // (e.g. on Windows where TempDir is inside %TEMP%, already writable).
        let overlaps_write = policy
            .write_paths
            .iter()
            .any(|w| canon.starts_with(w) || w.starts_with(&canon));
        if overlaps_write {
            assert!(
                !policy.read_paths.contains(&canon),
                "project root should NOT be in read_paths when covered by write_paths"
            );
        } else {
            assert!(
                policy.read_paths.contains(&canon),
                "project root should be in read_paths when WRITE not granted"
            );
        }
        assert!(
            !policy.write_paths.contains(&canon),
            "project root should NOT be in write_paths when WRITE not granted"
        );
    }

    #[test]
    fn test_build_sandbox_policy_allows_network() {
        let tmp = TempDir::new().unwrap();
        let policy = build_sandbox_policy(tmp.path(), ToolGrant::BASH).unwrap();
        assert!(policy.allow_network);
    }

    #[test]
    fn test_build_sandbox_policy_has_exec_paths() {
        let tmp = TempDir::new().unwrap();
        let policy = build_sandbox_policy(tmp.path(), ToolGrant::BASH).unwrap();
        assert!(
            !policy.exec_paths.is_empty(),
            "exec_paths should contain platform shell directories"
        );
    }

    #[test]
    fn test_classify_spawn_error_unsupported_is_setup_failed() {
        let e = lot::SandboxError::Unsupported("not supported".into());
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::SetupFailed(_)
        ));
    }

    #[test]
    fn test_classify_spawn_error_setup_is_setup_failed() {
        let e = lot::SandboxError::Setup("cannot create sandbox".into());
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::SetupFailed(_)
        ));
    }

    #[test]
    fn test_classify_spawn_error_invalid_policy_is_setup_failed() {
        let e = lot::SandboxError::InvalidPolicy("bad policy".into());
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::SetupFailed(_)
        ));
    }

    #[test]
    fn test_classify_spawn_error_io_permission_denied_is_setup_failed() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let e = lot::SandboxError::Io(io_err);
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::SetupFailed(_)
        ));
    }

    #[test]
    fn test_classify_spawn_error_io_not_found_is_other() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let e = lot::SandboxError::Io(io_err);
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::Other(_)
        ));
    }

    #[test]
    fn test_classify_spawn_error_cleanup_is_other() {
        let e = lot::SandboxError::Cleanup("cleanup failed".into());
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::Other(_)
        ));
    }

    #[test]
    fn test_classify_spawn_error_timeout_is_other() {
        let e = lot::SandboxError::Timeout(std::time::Duration::from_secs(5));
        assert!(matches!(
            classify_spawn_error(e),
            SandboxSpawnError::Other(_)
        ));
    }

    #[tokio::test]
    async fn test_bash_env_filtered() {
        let tmp = TempDir::new().unwrap();
        // SAFETY: no other threads read EPIC_TEST_SECRET.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("EPIC_TEST_SECRET", "leaked");
        }

        let result = execute_tool(
            "tu_1".into(),
            "bash",
            &serde_json::json!({"command": "env"}),
            tmp.path(),
            ToolGrant::BASH,
        )
        .await;

        assert!(!result.is_error);
        assert!(
            !result.content.contains("EPIC_TEST_SECRET"),
            "secret env var leaked into child process"
        );
        assert!(
            result.content.contains("PATH="),
            "PATH should be present in child environment"
        );

        // SAFETY: no other threads read EPIC_TEST_SECRET.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("EPIC_TEST_SECRET");
        }
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
        let result = execute_tool(
            "tu_1".into(),
            "read_file",
            &serde_json::json!({"path": "subdir"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "glob",
            &serde_json::json!({"pattern": "*.rs"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "grep",
            &serde_json::json!({"pattern": "zzz_no_match"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "grep",
            &serde_json::json!({"pattern": "match_me"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "edit_file",
            &serde_json::json!({"path": "f.txt", "old_string": "zzz", "new_string": "aaa"}),
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
        let result = execute_tool(
            "tu_1".into(),
            "edit_file",
            &serde_json::json!({"path": "f.txt"}),
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

    #[tokio::test]
    async fn test_write_file_overwrites() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "old content").unwrap();
        let result = execute_tool(
            "tu_1".into(),
            "write_file",
            &serde_json::json!({"path": "f.txt", "content": "new content"}),
            tmp.path(),
            ToolGrant::WRITE,
        )
        .await;
        assert!(!result.is_error);
        let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
        assert_eq!(content, "new content");
    }

    // -- bash edge cases --

    #[tokio::test]
    async fn test_bash_missing_command_param() {
        let result = exec("bash", serde_json::json!({}), ToolGrant::BASH).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn test_bash_stderr_output() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "echo errout >&2"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("[stderr]"));
        assert!(result.content.contains("errout"));
    }

    #[tokio::test]
    async fn test_bash_empty_output() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "true"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "[no output]");
    }

    #[tokio::test]
    async fn test_bash_mixed_stdout_stderr() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "echo out; echo err >&2"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("out"));
        assert!(result.content.contains("[stderr]"));
        assert!(result.content.contains("err"));
    }

    #[tokio::test]
    async fn test_bash_nonzero_with_stderr() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "echo fail >&2; exit 42"}),
            ToolGrant::BASH,
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("[stderr]"));
        assert!(result.content.contains("fail"));
        assert!(result.content.contains("[exit code: 42]"));
    }

    #[tokio::test]
    async fn test_bash_custom_timeout() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "echo fast", "timeout": 10}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("fast"));
    }

    #[tokio::test]
    async fn test_bash_timeout_clamped_to_max() {
        let result = exec(
            "bash",
            serde_json::json!({"command": "echo ok", "timeout": 99999}),
            ToolGrant::BASH,
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("ok"));
    }
}
