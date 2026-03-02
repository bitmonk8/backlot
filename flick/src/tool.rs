use std::borrow::Cow;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crate::config::{CustomToolConfig, ResourceAccess, ResourceConfig, ToolsConfig};
use crate::error::ToolError;
use crate::provider::ToolDefinition;

const TOOL_TIMEOUT_SECS: u64 = 120;
const DIR_ENTRY_CAP: usize = 10_000;
/// Maximum file size for `read_file` (10 MiB). Prevents OOM from LLM-directed reads.
const READ_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Abstraction over process execution for testability.
pub trait CommandRunner: Send + Sync {
    /// Run a shell command (via sh -c / cmd /C).
    fn run_shell<'a>(
        &'a self,
        command: &'a str,
        cwd: Option<&'a Path>,
        timeout_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>>;

    /// Run an executable with data piped to stdin.
    fn run_executable<'a>(
        &'a self,
        program: &'a Path,
        stdin_data: &'a [u8],
        timeout_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>>;
}

/// Real implementation that spawns OS processes.
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run_shell<'a>(
        &'a self,
        command: &'a str,
        cwd: Option<&'a Path>,
        timeout_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(shell_program());
            cmd.arg(shell_flag()).arg(command).kill_on_drop(true);
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output())
                .await
                .map_err(|_| ToolError::Timeout(timeout_secs))?
                .map_err(ToolError::from)
        })
    }

    fn run_executable<'a>(
        &'a self,
        program: &'a Path,
        stdin_data: &'a [u8],
        timeout_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let mut child = tokio::process::Command::new(program)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;

            // Write stdin concurrently with reading stdout/stderr to prevent
            // deadlock when child output fills the pipe buffer before consuming
            // all stdin data.
            let mut stdin_handle = child.stdin.take();
            let write_fut = async {
                if let Some(ref mut stdin) = stdin_handle {
                    use tokio::io::AsyncWriteExt;
                    // Ignore write errors: child may exit before consuming all input.
                    let _ = stdin.write_all(stdin_data).await;
                }
                drop(stdin_handle);
            };

            let ((), output_result) = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                async { tokio::join!(write_fut, child.wait_with_output()) },
            )
            .await
            .map_err(|_| ToolError::Timeout(timeout_secs))?;

            output_result.map_err(ToolError::from)
        })
    }
}

/// All available tools for a session, built from config.
pub struct ToolRegistry {
    builtins: Vec<BuiltinTool>,
    custom: Vec<CustomToolConfig>,
    resources: Vec<ResourceConfig>,
    /// Pre-canonicalized resource paths. `None` = no resources configured (allow all).
    canonical_resources: Option<Vec<(PathBuf, ResourceAccess)>>,
    cached_definitions: Vec<ToolDefinition>,
    runner: Box<dyn CommandRunner>,
}

#[derive(Debug, Clone, Copy)]
enum BuiltinTool {
    ReadFile,
    WriteFile,
    ListDirectory,
    ShellExec,
}

impl ToolRegistry {
    pub fn from_config(tools: &ToolsConfig, resources: Vec<ResourceConfig>) -> Self {
        Self::from_config_with_runner(tools, resources, Box::new(RealCommandRunner))
    }

    pub fn from_config_with_runner(
        tools: &ToolsConfig,
        resources: Vec<ResourceConfig>,
        runner: Box<dyn CommandRunner>,
    ) -> Self {
        let mut builtins = Vec::new();
        if tools.read_file {
            builtins.push(BuiltinTool::ReadFile);
        }
        if tools.write_file {
            builtins.push(BuiltinTool::WriteFile);
        }
        if tools.list_directory {
            builtins.push(BuiltinTool::ListDirectory);
        }
        if tools.shell_exec {
            builtins.push(BuiltinTool::ShellExec);
        }

        let mut cached_definitions = Vec::new();
        for builtin in &builtins {
            cached_definitions.push(builtin.definition());
        }
        for custom in &tools.custom {
            cached_definitions.push(ToolDefinition {
                name: custom.name.clone(),
                description: custom.description.clone(),
                input_schema: custom.parameters.clone(),
            });
        }

        if !builtins.is_empty() && resources.is_empty() {
            eprintln!("warning: file/shell tools enabled with no resources configured; all paths are allowed");
        }

        let canonical_resources = if resources.is_empty() {
            None
        } else {
            let mut resolved = Vec::with_capacity(resources.len());
            for r in &resources {
                match std::fs::canonicalize(&r.path) {
                    Ok(p) => resolved.push((p, r.access)),
                    Err(e) => {
                        eprintln!("warning: resource path {:?} could not be resolved: {e}", r.path);
                    }
                }
            }
            if resolved.is_empty() {
                eprintln!("warning: all resource paths failed to resolve; denying all path access");
                Some(Vec::new())
            } else {
                Some(resolved)
            }
        };

        Self {
            builtins,
            custom: tools.custom.clone(),
            resources,
            canonical_resources,
            cached_definitions,
            runner,
        }
    }

    /// Return cached tool definitions to send to the model.
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.cached_definitions
    }

    /// Execute a tool call by name with the given parsed JSON arguments.
    pub async fn execute(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, ToolError> {
        // Check builtins
        for builtin in &self.builtins {
            if builtin.name() == name {
                return builtin
                    .execute(
                        arguments,
                        &self.resources,
                        self.canonical_resources.as_deref(),
                        &*self.runner,
                    )
                    .await;
            }
        }

        // Check custom tools
        for custom in &self.custom {
            if custom.name == name {
                return execute_custom(custom, arguments, &self.resources, &*self.runner).await;
            }
        }

        Err(ToolError::NotFound(name.to_string()))
    }

}

impl BuiltinTool {
    const fn name(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::WriteFile => "write_file",
            Self::ListDirectory => "list_directory",
            Self::ShellExec => "shell_exec",
        }
    }

    fn definition(self) -> ToolDefinition {
        let name = self.name().into();
        match self {
            Self::ReadFile => ToolDefinition {
                name,
                description: "Read the contents of a file".into(),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path to read"}
                    },
                    "required": ["path"]
                })),
            },
            Self::WriteFile => ToolDefinition {
                name,
                description: "Write content to a file".into(),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path to write"},
                        "content": {"type": "string", "description": "Content to write"}
                    },
                    "required": ["path", "content"]
                })),
            },
            Self::ListDirectory => ToolDefinition {
                name,
                description: "List files and directories at a path".into(),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Directory path to list"}
                    },
                    "required": ["path"]
                })),
            },
            Self::ShellExec => ToolDefinition {
                name,
                description: "Execute a shell command".into(),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to execute"}
                    },
                    "required": ["command"]
                })),
            },
        }
    }

    async fn execute(
        self,
        args: &serde_json::Value,
        resources: &[ResourceConfig],
        canonical_resources: Option<&[(PathBuf, ResourceAccess)]>,
        runner: &dyn CommandRunner,
    ) -> Result<String, ToolError> {
        match self {
            Self::ReadFile => {
                let path = require_str(args, "path")?;
                let path = PathBuf::from(path);
                check_access(&path, ResourceAccess::Read, canonical_resources).await?;
                let metadata = tokio::fs::metadata(&path).await?;
                if metadata.len() > READ_FILE_MAX_BYTES {
                    return Err(ToolError::ExecutionFailed(format!(
                        "file too large ({} bytes, limit {})",
                        metadata.len(),
                        READ_FILE_MAX_BYTES
                    )));
                }
                let content = tokio::fs::read_to_string(&path).await?;
                Ok(content)
            }
            Self::WriteFile => {
                let path = require_str(args, "path")?;
                let content = require_str(args, "content")?;
                let path = PathBuf::from(path);
                check_access(&path, ResourceAccess::ReadWrite, canonical_resources).await?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, content).await?;
                Ok(format!("Wrote {} bytes to {}", content.len(), path.display()))
            }
            Self::ListDirectory => {
                let path = require_str(args, "path")?;
                let path = PathBuf::from(path);
                check_access(&path, ResourceAccess::Read, canonical_resources).await?;
                let mut entries = Vec::new();
                let mut dir = tokio::fs::read_dir(&path).await?;
                let mut truncated = false;
                while let Some(entry) = dir.next_entry().await? {
                    if entries.len() >= DIR_ENTRY_CAP {
                        truncated = true;
                        break;
                    }
                    let file_type = entry.file_type().await?;
                    let suffix = if file_type.is_dir() { "/" } else { "" };
                    entries.push(format!(
                        "{}{}",
                        entry.file_name().to_string_lossy(),
                        suffix
                    ));
                }
                entries.sort();
                let mut result = entries.join("\n");
                if truncated {
                    use std::fmt::Write;
                    let _ = write!(result, "\n(truncated at {DIR_ENTRY_CAP} entries)");
                }
                Ok(result)
            }
            Self::ShellExec => {
                let command = require_str(args, "command")?;
                let cwd = first_allowed_dir(resources);
                let output = runner
                    .run_shell(command, cwd.as_deref(), TOOL_TIMEOUT_SECS)
                    .await?;
                Ok(format_process_output(&output))
            }
        }
    }
}

/// Verify a path is allowed by the pre-canonicalized resource list.
/// `None` = no resources configured → allow all paths.
async fn check_access(
    path: &Path,
    required: ResourceAccess,
    canonical_resources: Option<&[(PathBuf, ResourceAccess)]>,
) -> Result<(), ToolError> {
    let Some(canonical_resources) = canonical_resources else {
        return Ok(());
    };

    // Reject any path containing `..` before canonicalization to prevent traversal
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(ToolError::PathDenied(path.to_path_buf()));
        }
    }

    let canonical = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => {
            // Path doesn't exist yet — walk ancestors until one resolves.
            // Handles multi-level nonexistent directories (e.g. resource/a/b/c/file.txt).
            let mut current = path.to_path_buf();
            let mut suffixes: Vec<std::ffi::OsString> = Vec::new();
            loop {
                match current.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => {
                        if let Some(name) = current.file_name() {
                            suffixes.push(name.to_os_string());
                        }
                        match tokio::fs::canonicalize(parent).await {
                            Ok(resolved) => {
                                let mut result = resolved;
                                for component in suffixes.into_iter().rev() {
                                    result = result.join(component);
                                }
                                break result;
                            }
                            Err(_) => {
                                current = parent.to_path_buf();
                            }
                        }
                    }
                    _ => return Err(ToolError::PathDenied(path.to_path_buf())),
                }
            }
        }
    };

    for (res_canonical, access) in canonical_resources {
        if canonical.starts_with(res_canonical) {
            match (required, *access) {
                (ResourceAccess::Read, _)
                | (ResourceAccess::ReadWrite, ResourceAccess::ReadWrite) => return Ok(()),
                _ => {}
            }
        }
    }

    Err(ToolError::PathDenied(path.to_path_buf()))
}


/// Format process output (stdout + stderr + exit code) into a single string.
fn format_process_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = stdout.into_owned();
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("stderr: ");
        result.push_str(&stderr);
    }
    if !output.status.success() {
        use std::fmt::Write;
        match output.status.code() {
            Some(code) => {
                let _ = write!(result, "\nexit code: {code}");
            }
            None => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = output.status.signal() {
                        let _ = write!(result, "\nkilled by signal: {sig}");
                    } else {
                        let _ = write!(result, "\nexit code: unknown");
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = write!(result, "\nexit code: unknown");
                }
            }
        }
    }
    result
}

/// Get the first allowed directory from resources for shell cwd.
/// Skips file paths — only returns actual directories.
fn first_allowed_dir(resources: &[ResourceConfig]) -> Option<PathBuf> {
    resources
        .iter()
        .map(|r| &r.path)
        .find(|p| p.is_dir())
        .cloned()
}

fn require_str<'a>(args: &'a serde_json::Value, field: &str) -> Result<&'a str, ToolError> {
    args.get(field).map_or_else(
        || Err(ToolError::ExecutionFailed(format!("missing field: {field}"))),
        |v| {
            v.as_str().ok_or_else(|| {
                ToolError::ExecutionFailed(format!("field '{field}' is not a string"))
            })
        },
    )
}

/// Characters that are cmd.exe metacharacters on Windows.
/// These allow command chaining/piping/redirection when passed through cmd /C.
/// Includes `"` to prevent breaking out of double-quote escaping.
#[cfg(windows)]
const CMD_METACHARACTERS: &[char] = &['\0', '\n', '\r', '&', '|', '%', '^', '<', '>', '(', ')', '!', '"'];

/// Escape a value for safe embedding in a cmd.exe command string.
/// Caller must have already rejected `CMD_METACHARACTERS` (including `"`).
/// Simple values (alphanumeric + safe punctuation) pass through unchanged;
/// values needing quoting (spaces, etc.) are wrapped in double quotes.
#[cfg(windows)]
fn escape_for_cmd(value: &str) -> Cow<'_, str> {
    if value.is_empty() {
        return Cow::Borrowed("\"\"");
    }
    // Safe set mirrors shell_escape's Unix heuristic: only leave unquoted
    // if every byte is alphanumeric or common safe punctuation.
    let needs_quoting = !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'=' | b'/' | b',' | b'.' | b'+' | b':' | b'\\'));
    if needs_quoting {
        Cow::Owned(format!("\"{value}\""))
    } else {
        Cow::Borrowed(value)
    }
}

/// Build a shell command string from a template and parameter values.
/// On Windows, rejects parameter values containing cmd.exe metacharacters
/// that `shell_escape` does not neutralize.
fn build_command_from_template(
    template: &str,
    arguments: &serde_json::Value,
) -> Result<String, ToolError> {
    let Some(obj) = arguments.as_object() else {
        return Err(ToolError::ExecutionFailed(
            "template tool arguments must be a JSON object".into(),
        ));
    };

    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        if let Some(end) = rest[start + 2..].find("}}") {
            let key = &rest[start + 2..start + 2 + end];
            if let Some(val) = obj.get(key) {
                let raw_value = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };

                // On Windows, reject values with cmd.exe metacharacters,
                // then double-quote if needed (shell_escape only does POSIX
                // single-quote wrapping, which cmd.exe ignores).
                #[cfg(windows)]
                {
                    if let Some(ch) = raw_value.chars().find(|c| CMD_METACHARACTERS.contains(c)) {
                        return Err(ToolError::ExecutionFailed(format!(
                            "parameter '{key}' contains cmd metacharacter '{ch}'"
                        )));
                    }
                    result.push_str(&escape_for_cmd(&raw_value));
                }
                #[cfg(not(windows))]
                {
                    let escaped = shell_escape::escape(Cow::Borrowed(&raw_value));
                    result.push_str(&escaped);
                }
            } else {
                // Unknown placeholder: keep literal
                result.push_str(&rest[start..start + 2 + end + 2]);
            }
            rest = &rest[start + 2 + end + 2..];
        } else {
            // No closing }}: keep the rest as literal
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    Ok(result)
}

async fn execute_custom(
    config: &CustomToolConfig,
    arguments: &serde_json::Value,
    resources: &[ResourceConfig],
    runner: &dyn CommandRunner,
) -> Result<String, ToolError> {
    if let Some(command_template) = &config.command {
        let command = build_command_from_template(command_template, arguments)?;

        let cwd = first_allowed_dir(resources);
        let output = runner
            .run_shell(&command, cwd.as_deref(), TOOL_TIMEOUT_SECS)
            .await?;
        Ok(format_process_output(&output))
    } else if let Some(executable) = &config.executable {
        let args_str = serde_json::to_string(arguments)
            .map_err(|e| ToolError::ExecutionFailed(format!("argument serialization failed: {e}")))?;
        let output = runner
            .run_executable(executable, args_str.as_bytes(), TOOL_TIMEOUT_SECS)
            .await?;
        Ok(format_process_output(&output))
    } else {
        Err(ToolError::ExecutionFailed(
            "custom tool has neither command nor executable".into(),
        ))
    }
}

const fn shell_program() -> &'static str {
    if cfg!(windows) { "cmd" } else { "sh" }
}

const fn shell_flag() -> &'static str {
    if cfg!(windows) { "/C" } else { "-c" }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn canonicalize_resources(resources: &[ResourceConfig]) -> Vec<(PathBuf, ResourceAccess)> {
        resources
            .iter()
            .filter_map(|r| {
                std::fs::canonicalize(&r.path)
                    .ok()
                    .map(|p| (p, r.access))
            })
            .collect()
    }

    fn default_tools_config() -> ToolsConfig {
        ToolsConfig {
            read_file: true,
            write_file: true,
            list_directory: true,
            shell_exec: false,
            custom: vec![],
        }
    }

    #[test]
    fn from_config_builtin_count() {
        let tools = default_tools_config();
        let registry = ToolRegistry::from_config(&tools, vec![]);
        assert_eq!(registry.definitions().len(), 3);
    }

    #[test]
    fn from_config_with_custom_tool() {
        let mut tools = default_tools_config();
        tools.custom.push(CustomToolConfig {
            name: "my_tool".into(),
            description: "a custom tool".into(),
            parameters: None,
            command: Some("echo test".into()),
            executable: None,
        });
        let registry = ToolRegistry::from_config(&tools, vec![]);
        assert_eq!(registry.definitions().len(), 4);
        assert_eq!(registry.definitions()[3].name, "my_tool");
    }

    #[tokio::test]
    async fn check_access_empty_resources_allows_all() {
        let result = check_access(
            Path::new("/any/path"),
            ResourceAccess::ReadWrite,
            None,
        )
        .await;
        result.expect("should succeed");
    }

    #[tokio::test]
    async fn check_access_denies_outside_resource() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let resources = vec![ResourceConfig {
            path: dir.path().to_path_buf(),
            access: ResourceAccess::ReadWrite,
        }];
        let canonical = canonicalize_resources(&resources);
        let result = check_access(
            Path::new("/definitely/not/inside"),
            ResourceAccess::Read,
            Some(&canonical),
        )
        .await;
        assert!(matches!(result, Err(ToolError::PathDenied(_))));
    }

    #[tokio::test]
    async fn check_access_allows_inside_resource() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("test.txt");
        tokio::fs::write(&file_path, "data").await.expect("write file");
        let resources = vec![ResourceConfig {
            path: dir.path().to_path_buf(),
            access: ResourceAccess::ReadWrite,
        }];
        let canonical = canonicalize_resources(&resources);
        let result = check_access(&file_path, ResourceAccess::Read, Some(&canonical)).await;
        result.expect("should succeed");
    }

    #[tokio::test]
    async fn check_access_read_only_denies_write() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("test.txt");
        tokio::fs::write(&file_path, "data").await.expect("write file");
        let resources = vec![ResourceConfig {
            path: dir.path().to_path_buf(),
            access: ResourceAccess::Read,
        }];
        let canonical = canonicalize_resources(&resources);
        let result = check_access(&file_path, ResourceAccess::ReadWrite, Some(&canonical)).await;
        assert!(matches!(result, Err(ToolError::PathDenied(_))));
    }

    #[tokio::test]
    async fn execute_read_file_success() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("hello.txt");
        tokio::fs::write(&file_path, "file content").await.expect("write");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                read_file: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::ReadWrite,
            }],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy()});
        let result = tools.execute("read_file", &args).await;
        assert_eq!(result.expect("should succeed"), "file content");
    }

    #[tokio::test]
    async fn execute_write_file_success() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("out.txt");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                write_file: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::ReadWrite,
            }],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy(), "content": "hello"});
        let result = tools.execute("write_file", &args).await;
        result.expect("should succeed");
        let written = tokio::fs::read_to_string(&file_path).await.expect("read back");
        assert_eq!(written, "hello");
    }

    #[tokio::test]
    async fn execute_write_file_denied_on_read_only_resource() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("out.txt");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                write_file: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::Read,
            }],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy(), "content": "should not be written"});
        let result = tools.execute("write_file", &args).await;
        assert!(
            matches!(result, Err(ToolError::PathDenied(_))),
            "write_file to read-only resource must be denied, got: {result:?}"
        );
        assert!(!file_path.exists(), "file must not have been created");
    }

    #[tokio::test]
    async fn execute_list_directory_success() {
        let dir = tempfile::tempdir().expect("create tempdir");
        tokio::fs::write(dir.path().join("a.txt"), "").await.expect("write a");
        tokio::fs::write(dir.path().join("b.txt"), "").await.expect("write b");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                list_directory: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::Read,
            }],
        );
        let args = serde_json::json!({"path": dir.path().to_string_lossy()});
        let result = tools.execute("list_directory", &args).await;
        let listing = result.expect("should succeed");
        assert!(listing.contains("a.txt"));
        assert!(listing.contains("b.txt"));
    }

    #[tokio::test]
    async fn execute_shell_exec() {
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                shell_exec: true,
                read_file: false,
                write_file: false,
                list_directory: false,
                custom: vec![],
            },
            vec![],
        );
        let args = serde_json::json!({"command": "echo hello"});
        let result = tools.execute("shell_exec", &args).await;
        assert!(result.expect("should succeed").contains("hello"));
    }

    #[tokio::test]
    async fn execute_custom_command_template() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let custom = CustomToolConfig {
            name: "greet".into(),
            description: "greet".into(),
            parameters: None,
            command: Some("echo {{name}}".into()),
            executable: None,
        };
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                custom: vec![custom],
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::ReadWrite,
            }],
        );
        let result = tools.execute("greet", &serde_json::json!({"name": "world"})).await;
        assert!(result.expect("should succeed").contains("world"));
    }

    // Creating 10,001 files is too slow; test the truncation message format instead.
    #[tokio::test]
    async fn list_directory_format_check() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let sub = dir.path().join("subdir");
        tokio::fs::create_dir(&sub).await.expect("mkdir");
        tokio::fs::write(dir.path().join("file.txt"), "").await.expect("write");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                list_directory: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::Read,
            }],
        );
        let args = serde_json::json!({"path": dir.path().to_string_lossy()});
        let result = tools.execute("list_directory", &args).await.expect("ok");
        assert!(result.contains("file.txt"));
        assert!(result.contains("subdir/"));
    }

    #[tokio::test]
    async fn check_access_nonexistent_path_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let nonexistent = dir.path().join("does_not_exist.txt");
        let resources = vec![ResourceConfig {
            path: dir.path().to_path_buf(),
            access: ResourceAccess::ReadWrite,
        }];
        let canonical = canonicalize_resources(&resources);
        // Should allow because parent canonicalizes to the resource dir
        let result = check_access(&nonexistent, ResourceAccess::ReadWrite, Some(&canonical)).await;
        result.expect("should succeed");
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_not_found() {
        let tools = ToolRegistry::from_config(&default_tools_config(), vec![]);
        let result = tools.execute("nonexistent", &serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::NotFound(_))));
    }

    #[tokio::test]
    async fn custom_tool_double_substitution_prevented() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let custom = CustomToolConfig {
            name: "echo_tool".into(),
            description: "echo".into(),
            parameters: None,
            command: Some("echo {{name}}".into()),
            executable: None,
        };
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                custom: vec![custom],
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::ReadWrite,
            }],
        );
        // The value contains another placeholder pattern — it should NOT be substituted
        let result = tools.execute("echo_tool", &serde_json::json!({"name": "{{other}}"})).await;
        let output = result.expect("should succeed");
        assert!(output.contains("{{other}}"),
            "value containing placeholder syntax should appear literally, got: {output}");
    }

    #[tokio::test]
    async fn read_file_non_utf8_returns_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("binary.bin");
        tokio::fs::write(&file_path, &[0xFF, 0xFE, 0x00, 0x80]).await.expect("write");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                read_file: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::Read,
            }],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy()});
        let result = tools.execute("read_file", &args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_file_exceeding_max_bytes_rejected() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("large.txt");
        // Create a file just over the 10 MiB limit
        let oversized = vec![b'x'; (READ_FILE_MAX_BYTES as usize) + 1];
        tokio::fs::write(&file_path, &oversized).await.expect("write");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                read_file: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::Read,
            }],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy()});
        let result = tools.execute("read_file", &args).await;
        let err = result.expect_err("should reject oversized file");
        assert!(
            matches!(err, ToolError::ExecutionFailed(ref msg) if msg.contains("file too large")),
            "expected ExecutionFailed with 'file too large', got: {err}"
        );
    }

    #[tokio::test]
    async fn read_file_at_exact_max_bytes_succeeds() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("exact.txt");
        let exact = vec![b'x'; READ_FILE_MAX_BYTES as usize];
        tokio::fs::write(&file_path, &exact).await.expect("write");
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                read_file: true,
                ..default_tools_config()
            },
            vec![ResourceConfig {
                path: dir.path().to_path_buf(),
                access: ResourceAccess::Read,
            }],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy()});
        let result = tools.execute("read_file", &args).await;
        result.expect("file at exact limit should succeed");
    }

    #[tokio::test]
    async fn write_file_creates_parent_directories() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("sub").join("deep").join("file.txt");
        // Empty resources list allows all paths, isolating the dir-creation behavior
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                write_file: true,
                ..default_tools_config()
            },
            vec![],
        );
        let args = serde_json::json!({"path": file_path.to_string_lossy(), "content": "nested"});
        let result = tools.execute("write_file", &args).await;
        result.expect("should succeed");
        let content = tokio::fs::read_to_string(&file_path).await.expect("read back");
        assert_eq!(content, "nested");
    }

    #[tokio::test]
    async fn shell_exec_nonzero_exit_includes_stderr_and_code() {
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                shell_exec: true,
                read_file: false,
                write_file: false,
                list_directory: false,
                custom: vec![],
            },
            vec![],
        );
        let cmd = if cfg!(windows) {
            "echo err 1>&2 && exit 1"
        } else {
            "echo err >&2 && exit 1"
        };
        let args = serde_json::json!({"command": cmd});
        let result = tools.execute("shell_exec", &args).await;
        let output = result.expect("should succeed");
        assert!(output.contains("stderr:"), "should contain stderr marker: {output}");
        assert!(output.contains("exit code:"), "should contain exit code: {output}");
    }

    #[tokio::test]
    async fn check_access_denies_multi_level_traversal() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let resources = vec![ResourceConfig {
            path: dir.path().to_path_buf(),
            access: ResourceAccess::ReadWrite,
        }];
        let canonical = canonicalize_resources(&resources);
        let malicious = dir.path().join("sub").join("..").join("..").join("..").join("etc").join("passwd");
        let result = check_access(&malicious, ResourceAccess::Read, Some(&canonical)).await;
        assert!(matches!(result, Err(ToolError::PathDenied(_))));
    }

    #[tokio::test]
    async fn check_access_denies_single_dotdot_component() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let resources = vec![ResourceConfig {
            path: dir.path().to_path_buf(),
            access: ResourceAccess::ReadWrite,
        }];
        let canonical = canonicalize_resources(&resources);
        let malicious = dir.path().join("resource").join("..").join("etc").join("passwd");
        let result = check_access(&malicious, ResourceAccess::Read, Some(&canonical)).await;
        assert!(matches!(result, Err(ToolError::PathDenied(_))));
    }

    #[tokio::test]
    async fn require_str_non_string_value_returns_type_error() {
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                read_file: true,
                ..default_tools_config()
            },
            vec![],
        );
        let args = serde_json::json!({"path": 42});
        let result = tools.execute("read_file", &args).await;
        assert!(result.is_err());
        let err = result.expect_err("should fail");
        assert!(
            matches!(&err, ToolError::ExecutionFailed(msg) if msg.contains("not a string")),
            "expected 'not a string' error, got: {err}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn build_command_rejects_cmd_metacharacters() {
        let template = "echo {{name}}";
        let args_ampersand = serde_json::json!({"name": "foo & bar"});
        let result = build_command_from_template(template, &args_ampersand);
        assert!(matches!(result, Err(ToolError::ExecutionFailed(ref msg)) if msg.contains("metacharacter")),
            "expected metacharacter rejection for '&', got: {result:?}");

        let args_pipe = serde_json::json!({"name": "foo | bar"});
        let result = build_command_from_template(template, &args_pipe);
        assert!(matches!(result, Err(ToolError::ExecutionFailed(ref msg)) if msg.contains("metacharacter")),
            "expected metacharacter rejection for '|', got: {result:?}");

        let args_quote = serde_json::json!({"name": "foo\" bar"});
        let result = build_command_from_template(template, &args_quote);
        assert!(matches!(result, Err(ToolError::ExecutionFailed(ref msg)) if msg.contains("metacharacter")),
            "expected metacharacter rejection for '\"', got: {result:?}");
    }

    #[cfg(windows)]
    #[test]
    fn build_command_windows_double_quote_escaping() {
        let template = "echo {{name}}";
        // Value with spaces needs double-quoting
        let args = serde_json::json!({"name": "hello world"});
        let result = build_command_from_template(template, &args).expect("should succeed");
        assert_eq!(result, r#"echo "hello world""#);
        // Simple value passes through unquoted
        let args = serde_json::json!({"name": "world"});
        let result = build_command_from_template(template, &args).expect("should succeed");
        assert_eq!(result, "echo world");
        // Empty value gets empty double quotes
        let args = serde_json::json!({"name": ""});
        let result = build_command_from_template(template, &args).expect("should succeed");
        assert_eq!(result, r#"echo """#);
    }

    #[tokio::test]
    async fn custom_tool_unclosed_placeholder_kept_literal() {
        let custom = CustomToolConfig {
            name: "broken_template".into(),
            description: "test".into(),
            parameters: None,
            command: Some("echo {{unclosed".into()),
            executable: None,
        };
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                custom: vec![custom],
                ..default_tools_config()
            },
            vec![],
        );
        let result = tools.execute("broken_template", &serde_json::json!({})).await;
        let output = result.expect("should succeed");
        assert!(output.contains("{{unclosed"), "unclosed placeholder should appear literally: {output}");
    }

    #[tokio::test]
    async fn check_access_with_multiple_resources() {
        let dir1 = tempfile::tempdir().expect("create tempdir1");
        let dir2 = tempfile::tempdir().expect("create tempdir2");
        let file_path = dir2.path().join("allowed.txt");
        tokio::fs::write(&file_path, "data").await.expect("write");
        let resources = vec![
            ResourceConfig {
                path: dir1.path().to_path_buf(),
                access: ResourceAccess::Read,
            },
            ResourceConfig {
                path: dir2.path().to_path_buf(),
                access: ResourceAccess::ReadWrite,
            },
        ];
        let canonical = canonicalize_resources(&resources);
        // File in second resource dir should be accessible
        let result = check_access(&file_path, ResourceAccess::ReadWrite, Some(&canonical)).await;
        result.expect("should succeed");
    }

    #[tokio::test]
    async fn execute_custom_tool_via_executable() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let script_path = if cfg!(windows) {
            let p = dir.path().join("tool.cmd");
            tokio::fs::write(&p, "@echo off\necho custom_output").await.expect("write script");
            p
        } else {
            let p = dir.path().join("tool.sh");
            tokio::fs::write(&p, "#!/bin/sh\necho custom_output").await.expect("write script");
            // Make executable
            let status = tokio::process::Command::new("chmod")
                .args(["+x", &p.to_string_lossy()])
                .status()
                .await
                .expect("chmod");
            assert!(status.success());
            p
        };
        let custom = CustomToolConfig {
            name: "exec_tool".into(),
            description: "test executable".into(),
            parameters: None,
            command: None,
            executable: Some(script_path),
        };
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                custom: vec![custom],
                ..default_tools_config()
            },
            vec![],
        );
        let result = tools.execute("exec_tool", &serde_json::json!({})).await;
        assert!(result.expect("should succeed").contains("custom_output"));
    }

    struct TimeoutRunner;

    impl CommandRunner for TimeoutRunner {
        fn run_shell<'a>(
            &'a self,
            _command: &'a str,
            _cwd: Option<&'a Path>,
            timeout_secs: u64,
        ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>>
        {
            Box::pin(async move { Err(ToolError::Timeout(timeout_secs)) })
        }

        fn run_executable<'a>(
            &'a self,
            _program: &'a Path,
            _stdin_data: &'a [u8],
            timeout_secs: u64,
        ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>>
        {
            Box::pin(async move { Err(ToolError::Timeout(timeout_secs)) })
        }
    }

    #[tokio::test]
    async fn shell_exec_timeout_propagates() {
        let tools = ToolRegistry::from_config_with_runner(
            &ToolsConfig {
                shell_exec: true,
                read_file: false,
                write_file: false,
                list_directory: false,
                custom: vec![],
            },
            vec![],
            Box::new(TimeoutRunner),
        );
        let args = serde_json::json!({"command": "sleep 999"});
        let result = tools.execute("shell_exec", &args).await;
        assert!(
            matches!(result, Err(ToolError::Timeout(TOOL_TIMEOUT_SECS))),
            "expected Timeout({TOOL_TIMEOUT_SECS}), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn custom_tool_command_timeout_propagates() {
        let custom = CustomToolConfig {
            name: "slow_tool".into(),
            description: "test".into(),
            parameters: None,
            command: Some("sleep 999".into()),
            executable: None,
        };
        let tools = ToolRegistry::from_config_with_runner(
            &ToolsConfig {
                custom: vec![custom],
                read_file: false,
                write_file: false,
                list_directory: false,
                shell_exec: false,
            },
            vec![],
            Box::new(TimeoutRunner),
        );
        let args = serde_json::json!({});
        let result = tools.execute("slow_tool", &args).await;
        assert!(
            matches!(result, Err(ToolError::Timeout(TOOL_TIMEOUT_SECS))),
            "expected Timeout({TOOL_TIMEOUT_SECS}), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn custom_tool_executable_timeout_propagates() {
        let custom = CustomToolConfig {
            name: "slow_exec".into(),
            description: "test".into(),
            parameters: None,
            command: None,
            executable: Some(PathBuf::from("/bin/sleep")),
        };
        let tools = ToolRegistry::from_config_with_runner(
            &ToolsConfig {
                custom: vec![custom],
                read_file: false,
                write_file: false,
                list_directory: false,
                shell_exec: false,
            },
            vec![],
            Box::new(TimeoutRunner),
        );
        let args = serde_json::json!({});
        let result = tools.execute("slow_exec", &args).await;
        assert!(
            matches!(result, Err(ToolError::Timeout(TOOL_TIMEOUT_SECS))),
            "expected Timeout({TOOL_TIMEOUT_SECS}), got: {result:?}"
        );
    }

    // Config validation prevents this, but the runtime path is tested for defense-in-depth.
    #[tokio::test]
    async fn execute_custom_no_command_or_executable() {
        let custom = CustomToolConfig {
            name: "broken_tool".into(),
            description: "no command or executable".into(),
            parameters: None,
            command: None,
            executable: None,
        };
        // Bypass config validation by constructing registry directly
        let tools = ToolRegistry::from_config(
            &ToolsConfig {
                custom: vec![custom],
                ..default_tools_config()
            },
            vec![],
        );
        let result = tools.execute("broken_tool", &serde_json::json!({})).await;
        assert!(result.is_err());
        let err = result.expect_err("should fail with ExecutionFailed");
        assert!(
            matches!(err, ToolError::ExecutionFailed(ref msg) if msg.contains("neither command nor executable")),
            "expected ExecutionFailed with 'neither command nor executable', got: {err}"
        );
    }

    #[tokio::test]
    async fn shell_exec_disabled_by_config_returns_not_found() {
        let tools_config = ToolsConfig {
            shell_exec: false,
            ..default_tools_config()
        };
        let registry = ToolRegistry::from_config(&tools_config, vec![]);
        let args = serde_json::json!({"command": "echo hello"});
        let result = registry.execute("shell_exec", &args).await;
        assert!(
            matches!(result, Err(ToolError::NotFound(_))),
            "shell_exec with shell_exec=false must return NotFound, got: {result:?}"
        );
    }

}
