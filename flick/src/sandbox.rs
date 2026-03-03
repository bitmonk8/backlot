use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crate::config::{ResourceAccess, ResourceConfig, SandboxConfig};
use crate::error::ToolError;
use crate::tool::CommandRunner;

/// Replace `{cwd}`, `{path}`, `{policy_file}`, `{pid}` in a template string.
/// Uses single-pass scanning to prevent substituted values from being
/// re-expanded by later replacements. Unknown placeholders are left as-is.
#[allow(clippy::literal_string_with_formatting_args)]
pub fn expand_placeholders(
    template: &str,
    cwd: &str,
    path: &str,
    policy_file: &str,
) -> String {
    let pid = std::process::id().to_string();
    let replacements: &[(&str, &str)] = &[
        ("{cwd}", cwd),
        ("{path}", path),
        ("{policy_file}", policy_file),
        ("{pid}", &pid),
    ];
    single_pass_replace(template, replacements)
}

/// Single-pass placeholder replacement. Scans left-to-right, replacing the
/// first matching placeholder at each `{` position. Already-substituted text
/// is never re-scanned, preventing second-order expansion.
fn single_pass_replace(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(pos) = remaining.find('{') {
        result.push_str(&remaining[..pos]);
        let after_brace = &remaining[pos..];

        let mut matched = false;
        for &(placeholder, value) in replacements {
            if after_brace.starts_with(placeholder) {
                result.push_str(value);
                remaining = &after_brace[placeholder.len()..];
                matched = true;
                break;
            }
        }
        if !matched {
            // Not a known placeholder — emit the `{` and advance past it
            result.push('{');
            remaining = &after_brace[1..];
        }
    }
    result.push_str(remaining);
    result
}

/// Resolve a resource path to an absolute string. Uses canonicalize if possible,
/// falls back to joining with cwd for paths that don't exist yet.
/// On Windows, strips the `\\?\` UNC prefix that canonicalize adds, since
/// sandbox tools may not understand extended-length paths.
fn resolve_resource_path(path: &Path, cwd: &str) -> String {
    if let Ok(canon) = std::fs::canonicalize(path) {
        let s = canon.to_string_lossy().into_owned();
        return strip_unc_prefix(s);
    }
    if path.is_absolute() {
        return path.to_string_lossy().into_owned();
    }
    PathBuf::from(cwd).join(path).to_string_lossy().into_owned()
}

/// Strip the `\\?\` extended-length prefix on Windows. No-op on other platforms.
fn strip_unc_prefix(path: String) -> String {
    if cfg!(windows) {
        path.strip_prefix(r"\\?\").map(String::from).unwrap_or(path)
    } else {
        path
    }
}

/// Build the full prefix command from sandbox config and resources.
///
/// Result: `[wrapper...] [read_args per read resource...] [rw_args per rw resource...] [suffix...]`
pub fn build_prefix(
    sandbox: &SandboxConfig,
    resources: &[ResourceConfig],
    cwd: &str,
    policy_file_path: &str,
) -> Vec<String> {
    let mut prefix = Vec::new();

    // Expand wrapper args
    for arg in sandbox.wrapper() {
        prefix.push(expand_placeholders(arg, cwd, "", policy_file_path));
    }

    // Per-resource read args
    if !sandbox.read_args().is_empty() {
        for res in resources {
            if res.access == ResourceAccess::Read {
                let path_str = resolve_resource_path(&res.path, cwd);
                for arg in sandbox.read_args() {
                    prefix.push(expand_placeholders(arg, cwd, &path_str, policy_file_path));
                }
            }
        }
    }

    // Per-resource read_write args
    if !sandbox.read_write_args().is_empty() {
        for res in resources {
            if res.access == ResourceAccess::ReadWrite {
                let path_str = resolve_resource_path(&res.path, cwd);
                for arg in sandbox.read_write_args() {
                    prefix.push(expand_placeholders(arg, cwd, &path_str, policy_file_path));
                }
            }
        }
    }

    // Suffix
    for arg in sandbox.suffix() {
        prefix.push(expand_placeholders(arg, cwd, "", policy_file_path));
    }

    prefix
}

/// Generate policy file content by expanding all placeholders in one shot.
///
/// Per-rule templates get `{path}` and `{cwd}` expanded via single-pass replace.
/// The assembled rules are then inserted into the template alongside `{cwd}`,
/// `{policy_file}`, and `{pid}`. All expansion happens in a single pass per
/// stage, and substituted values are never re-scanned, preventing second-order
/// expansion of placeholder-like patterns in resource paths.
pub fn generate_policy_content(
    template: &str,
    read_rule: Option<&str>,
    rw_rule: Option<&str>,
    resources: &[ResourceConfig],
    cwd: &str,
    policy_file: &str,
) -> String {
    let pid = std::process::id().to_string();
    let mut read_lines = Vec::new();
    let mut rw_lines = Vec::new();

    for res in resources {
        let path_str = resolve_resource_path(&res.path, cwd);
        match res.access {
            ResourceAccess::Read => {
                if let Some(rule) = read_rule {
                    read_lines.push(single_pass_replace(rule, &[
                        ("{path}", &path_str),
                        ("{cwd}", cwd),
                    ]));
                }
            }
            ResourceAccess::ReadWrite => {
                if let Some(rule) = rw_rule {
                    rw_lines.push(single_pass_replace(rule, &[
                        ("{path}", &path_str),
                        ("{cwd}", cwd),
                    ]));
                }
            }
        }
    }

    let read_rules_str = read_lines.join("\n");
    let rw_rules_str = rw_lines.join("\n");
    single_pass_replace(template, &[
        ("{read_rules}", &read_rules_str),
        ("{read_write_rules}", &rw_rules_str),
        ("{cwd}", cwd),
        ("{policy_file}", policy_file),
        ("{pid}", &pid),
    ])
}

/// Validate that the wrapper program exists in PATH or as an absolute path.
/// Returns the resolved path on success, or an error message on failure.
pub fn validate_wrapper(program: &str) -> Result<PathBuf, String> {
    let path = Path::new(program);

    // Absolute path: check existence directly
    if path.is_absolute() {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(format!("sandbox wrapper not found: {program}"))
        };
    }

    // Relative name: search PATH
    let path_var = std::env::var("PATH")
        .map_err(|_| format!("PATH not set, cannot find: {program}"))?;

    let separator = if cfg!(windows) { ';' } else { ':' };

    // Read PATHEXT once before the loop (Windows only)
    let pathext = if cfg!(windows) { std::env::var("PATHEXT").ok() } else { None };

    for dir in path_var.split(separator) {
        let candidate = PathBuf::from(dir).join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
        // On Windows, also try PATHEXT extensions
        if let Some(ref pathext) = pathext {
            for ext in pathext.split(';') {
                let mut with_ext = candidate.as_os_str().to_os_string();
                with_ext.push(ext);
                let candidate_ext = PathBuf::from(with_ext);
                if candidate_ext.is_file() {
                    return Ok(candidate_ext);
                }
            }
        }
    }

    Err(format!("sandbox wrapper not found in PATH: {program}"))
}

/// Write policy file content to disk.
pub fn write_policy_file(path: &Path, content: &str) -> Result<(), std::io::Error> {
    std::fs::write(path, content)
}

/// Decorator that prepends a sandbox wrapper prefix to all subprocess invocations.
///
/// Bypasses the inner runner entirely — spawns `prefix[0]` directly with
/// the rest of the prefix as args, followed by the shell/program args.
pub struct SandboxCommandRunner {
    prefix: Vec<String>,
}

impl SandboxCommandRunner {
    pub fn new(prefix: Vec<String>) -> Self {
        debug_assert!(!prefix.is_empty(), "sandbox prefix must not be empty");
        Self { prefix }
    }
}

impl CommandRunner for SandboxCommandRunner {
    fn run_shell<'a>(
        &'a self,
        command: &'a str,
        cwd: Option<&'a Path>,
        timeout_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Result<std::process::Output, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            // Spawn prefix[0] with [prefix[1..], shell, -c, command] as args.
            // Bypasses inner runner to avoid double shell wrapping.
            let shell = if cfg!(windows) { "cmd" } else { "sh" };
            let shell_flag = if cfg!(windows) { "/C" } else { "-c" };

            let mut cmd = tokio::process::Command::new(&self.prefix[0]);
            for arg in &self.prefix[1..] {
                cmd.arg(arg);
            }
            cmd.arg(shell).arg(shell_flag).arg(command);
            cmd.kill_on_drop(true);
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
            // Spawn prefix[0] with [prefix[1..], program] as args, piping stdin.
            let mut child = tokio::process::Command::new(&self.prefix[0]);
            for arg in &self.prefix[1..] {
                child.arg(arg);
            }
            child.arg(program.as_os_str());
            child
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);

            let mut spawned = child.spawn()?;

            let mut stdin_handle = spawned.stdin.take();
            let write_fut = async {
                if let Some(ref mut stdin) = stdin_handle {
                    use tokio::io::AsyncWriteExt;
                    let _ = stdin.write_all(stdin_data).await;
                }
                drop(stdin_handle);
            };

            let ((), output_result) = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                async { tokio::join!(write_fut, spawned.wait_with_output()) },
            )
            .await
            .map_err(|_| ToolError::Timeout(timeout_secs))?;

            output_result.map_err(ToolError::from)
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    // -- Placeholder expansion tests --

    #[test]
    fn expand_all_placeholders() {
        let result = expand_placeholders(
            "--chdir {cwd} --path {path} --policy {policy_file} --pid {pid}",
            "/home/user",
            "/tmp/data",
            "/tmp/policy.sb",
        );
        assert!(result.contains("/home/user"));
        assert!(result.contains("/tmp/data"));
        assert!(result.contains("/tmp/policy.sb"));
        assert!(result.contains(&std::process::id().to_string()));
    }

    #[test]
    fn expand_partial_placeholders() {
        let result = expand_placeholders("--bind {path} {path}", "/cwd", "/data", "");
        assert_eq!(result, "--bind /data /data");
    }

    #[test]
    fn expand_no_placeholders() {
        let result = expand_placeholders("--die-with-parent", "/cwd", "/p", "/pf");
        assert_eq!(result, "--die-with-parent");
    }

    #[test]
    fn expand_unknown_placeholder_left_as_is() {
        let result = expand_placeholders("{unknown}", "/cwd", "/p", "/pf");
        assert_eq!(result, "{unknown}");
    }

    #[test]
    fn expand_no_second_order_expansion() {
        // Value contains another placeholder — must NOT be re-expanded.
        let result = expand_placeholders("{path}", "/cwd", "{cwd}", "/pf");
        assert_eq!(result, "{cwd}", "substituted value must not be re-expanded");
    }

    #[test]
    fn expand_adjacent_braces() {
        let result = expand_placeholders("{cwd}{path}", "/home", "/data", "");
        assert_eq!(result, "/home/data");
    }

    // -- Policy content generation tests --

    #[test]
    fn generate_policy_read_and_rw() {
        let resources = vec![
            ResourceConfig { path: PathBuf::from("/data"), access: ResourceAccess::Read },
            ResourceConfig { path: PathBuf::from("/work"), access: ResourceAccess::ReadWrite },
        ];
        let content = generate_policy_content(
            "(version 1)\n{read_rules}\n{read_write_rules}",
            Some("(read \"{path}\")"),
            Some("(rw \"{path}\")"),
            &resources,
            "/home",
            "",
        );
        assert!(content.contains("(read \"/data\")"));
        assert!(content.contains("(rw \"/work\")"));
    }

    #[test]
    fn generate_policy_no_resources() {
        let content = generate_policy_content(
            "(version 1)\n{read_rules}\n{read_write_rules}",
            Some("(read \"{path}\")"),
            Some("(rw \"{path}\")"),
            &[],
            "/home",
            "",
        );
        assert!(content.contains("(version 1)"));
        assert!(!content.contains("(read"));
        assert!(!content.contains("(rw"));
    }

    #[test]
    fn generate_policy_read_only() {
        let resources = vec![
            ResourceConfig { path: PathBuf::from("/ro"), access: ResourceAccess::Read },
        ];
        let content = generate_policy_content(
            "{read_rules}\n{read_write_rules}",
            Some("(read \"{path}\")"),
            None,
            &resources,
            "/home",
            "",
        );
        assert!(content.contains("(read \"/ro\")"));
    }

    #[test]
    fn generate_policy_relative_path_absolutized() {
        // Use a non-existent relative path so canonicalize falls back to cwd join.
        let resources = vec![
            ResourceConfig { path: PathBuf::from("nonexistent_dir"), access: ResourceAccess::ReadWrite },
        ];
        let cwd = "/home/user/project";
        let content = generate_policy_content(
            "{read_write_rules}",
            None,
            Some("(rw \"{path}\")"),
            &resources,
            cwd,
            "",
        );
        let expected = PathBuf::from(cwd).join("nonexistent_dir");
        let expected_str = expected.to_string_lossy();
        assert!(content.contains(&*expected_str), "content: {content}");
    }

    #[test]
    fn generate_policy_no_second_order_expansion() {
        // A resource path containing {read_write_rules} must not inject into the template.
        let resources = vec![
            ResourceConfig { path: PathBuf::from("/data/{read_write_rules}"), access: ResourceAccess::Read },
            ResourceConfig { path: PathBuf::from("/work"), access: ResourceAccess::ReadWrite },
        ];
        let content = generate_policy_content(
            "{read_rules}\n{read_write_rules}",
            Some("(read \"{path}\")"),
            Some("(rw \"{path}\")"),
            &resources,
            "/home",
            "",
        );
        // The read rule should contain the literal path, not expanded rw rules
        assert!(content.contains("(read \"/data/{read_write_rules}\")"), "content: {content}");
        assert!(content.contains("(rw \"/work\")"), "content: {content}");
    }

    #[test]
    fn generate_policy_no_cross_function_expansion() {
        // A resource path containing {cwd} must not be expanded by the
        // template-level placeholder pass (the old two-step bug).
        let resources = vec![
            ResourceConfig { path: PathBuf::from("/data/{cwd}/thing"), access: ResourceAccess::Read },
        ];
        let content = generate_policy_content(
            "{read_rules}\n{read_write_rules}",
            Some("(read \"{path}\")"),
            None,
            &resources,
            "/home",
            "",
        );
        assert!(
            content.contains("(read \"/data/{cwd}/thing\")"),
            "{{cwd}} inside resource path must NOT be expanded: {content}"
        );
    }

    #[test]
    fn generate_policy_expands_cwd_and_policy_file_in_template() {
        let content = generate_policy_content(
            "(version 1)\n(cwd {cwd})\n(pf {policy_file})\n{read_rules}\n{read_write_rules}",
            None,
            None,
            &[],
            "/home/user",
            "/tmp/policy.sb",
        );
        assert!(content.contains("(cwd /home/user)"), "content: {content}");
        assert!(content.contains("(pf /tmp/policy.sb)"), "content: {content}");
    }

    #[test]
    fn strip_unc_prefix_windows_path() {
        let result = strip_unc_prefix(r"\\?\C:\data".to_string());
        if cfg!(windows) {
            assert_eq!(result, r"C:\data");
        } else {
            assert_eq!(result, r"\\?\C:\data");
        }
    }

    #[test]
    fn strip_unc_prefix_normal_path() {
        let result = strip_unc_prefix("/home/user".to_string());
        assert_eq!(result, "/home/user");
    }

    // -- Prefix building tests --

    #[test]
    fn build_prefix_bwrap_style() {
        let sandbox = make_sandbox_config(
            vec!["bwrap".into(), "--die-with-parent".into()],
            vec!["--ro-bind".into(), "{path}".into(), "{path}".into()],
            vec!["--bind".into(), "{path}".into(), "{path}".into()],
            vec!["--".into()],
        );
        let resources = vec![
            ResourceConfig { path: PathBuf::from("/data"), access: ResourceAccess::Read },
            ResourceConfig { path: PathBuf::from("/work"), access: ResourceAccess::ReadWrite },
        ];
        let prefix = build_prefix(&sandbox, &resources, "/home", "");
        assert_eq!(prefix, vec![
            "bwrap", "--die-with-parent",
            "--ro-bind", "/data", "/data",
            "--bind", "/work", "/work",
            "--",
        ]);
    }

    #[test]
    fn build_prefix_no_read_args() {
        let sandbox = make_sandbox_config(
            vec!["sandbox-exec".into(), "-f".into(), "{policy_file}".into()],
            vec![],
            vec![],
            vec!["--".into()],
        );
        let prefix = build_prefix(&sandbox, &[], "/home", "/tmp/policy.sb");
        assert_eq!(prefix, vec!["sandbox-exec", "-f", "/tmp/policy.sb", "--"]);
    }

    #[test]
    fn build_prefix_policy_file_in_wrapper() {
        let sandbox = make_sandbox_config(
            vec!["sandbox-exec".into(), "-f".into(), "{policy_file}".into()],
            vec![],
            vec![],
            vec![],
        );
        let prefix = build_prefix(&sandbox, &[], "/cwd", "/tmp/p.sb");
        assert_eq!(prefix[2], "/tmp/p.sb");
    }

    #[test]
    fn build_prefix_relative_path_absolutized() {
        let sandbox = make_sandbox_config(
            vec!["bwrap".into()],
            vec!["--ro-bind".into(), "{path}".into(), "{path}".into()],
            vec![],
            vec![],
        );
        // Use a non-existent relative path so canonicalize falls back to cwd join.
        let cwd = "/home/user/project";
        let resources = vec![
            ResourceConfig { path: PathBuf::from("nonexistent_dir"), access: ResourceAccess::Read },
        ];
        let prefix = build_prefix(&sandbox, &resources, cwd, "");
        let expected_path = PathBuf::from(cwd).join("nonexistent_dir").to_string_lossy().into_owned();
        assert_eq!(prefix, vec!["bwrap", "--ro-bind", &expected_path, &expected_path]);
    }

    // -- Wrapper validation tests --

    #[test]
    fn validate_wrapper_nonexistent_returns_error() {
        let result = validate_wrapper("nonexistent_binary_xyz_123");
        assert!(result.is_err());
        assert!(result.as_ref().err().unwrap().contains("not found"));
    }

    #[test]
    fn validate_wrapper_absolute_nonexistent_returns_error() {
        let result = validate_wrapper("/nonexistent/path/to/binary");
        assert!(result.is_err());
        assert!(result.as_ref().err().unwrap().contains("not found"));
    }

    #[test]
    fn validate_wrapper_empty_string_returns_error() {
        let result = validate_wrapper("");
        assert!(result.is_err(), "empty string should not resolve: {result:?}");
    }

    // -- Wrapper validation: positive case --

    #[test]
    fn validate_wrapper_existing_binary_found() {
        // "cargo" is guaranteed to be in PATH during `cargo test`
        let result = validate_wrapper("cargo");
        assert!(result.is_ok(), "cargo should be found in PATH: {result:?}");
    }

    // -- write_policy_file round-trip --

    #[test]
    fn write_policy_file_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("policy.sb");
        let content = "(version 1)\n(deny default)";
        write_policy_file(&path, content).expect("write should succeed");
        let read_back = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(read_back, content);
    }

    // -- SandboxCommandRunner tests --

    #[tokio::test]
    async fn sandbox_runner_run_shell_prepends_prefix() {
        // Use echo as the wrapper — it prints all args, proving the prefix is prepended.
        // On Windows, use "cmd /C echo PREFIX_MARKER" so the wrapper prints the
        // marker plus trailing args (the inner shell invocation) and exits 0.
        let runner = if cfg!(windows) {
            SandboxCommandRunner::new(vec![
                "cmd".to_string(), "/C".to_string(),
                "echo".to_string(), "PREFIX_MARKER".to_string(),
            ])
        } else {
            SandboxCommandRunner::new(vec![
                "echo".to_string(), "PREFIX_MARKER".to_string(),
            ])
        };
        let output = runner.run_shell("hello", None, 10).await.expect("should succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("PREFIX_MARKER"), "stdout: {stdout}");
    }

    #[tokio::test]
    async fn sandbox_runner_run_executable_prepends_prefix() {
        let runner = if cfg!(windows) {
            SandboxCommandRunner::new(vec![
                "cmd".to_string(), "/C".to_string(),
                "echo".to_string(), "EXEC_MARKER".to_string(),
            ])
        } else {
            SandboxCommandRunner::new(vec![
                "echo".to_string(), "EXEC_MARKER".to_string(),
            ])
        };
        let program = if cfg!(windows) {
            Path::new("echo.exe")
        } else {
            Path::new("/bin/sh")
        };
        let output = runner.run_executable(program, b"", 10).await.expect("should succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("EXEC_MARKER"), "stdout: {stdout}");
    }

    // -- Helper to build SandboxConfig for tests --

    fn make_sandbox_config(
        wrapper: Vec<String>,
        read_args: Vec<String>,
        read_write_args: Vec<String>,
        suffix: Vec<String>,
    ) -> SandboxConfig {
        // Parse from TOML to respect private fields
        let mut toml_str = format!(
            "wrapper = {}\n",
            format_toml_array(&wrapper),
        );
        if !read_args.is_empty() {
            toml_str.push_str(&format!("read_args = {}\n", format_toml_array(&read_args)));
        }
        if !read_write_args.is_empty() {
            toml_str.push_str(&format!("read_write_args = {}\n", format_toml_array(&read_write_args)));
        }
        if !suffix.is_empty() {
            toml_str.push_str(&format!("suffix = {}\n", format_toml_array(&suffix)));
        }
        toml::from_str(&toml_str).expect("test sandbox config should parse")
    }

    fn format_toml_array(items: &[String]) -> String {
        let escaped: Vec<String> = items.iter().map(|s| {
            let s = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{s}\"")
        }).collect();
        format!("[{}]", escaped.join(", "))
    }
}
