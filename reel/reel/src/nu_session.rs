// MCP client for a persistent `nu --mcp` process.
//
// Manages the lifecycle of one NuShell MCP server process per agent session.
// The process is spawned eagerly at session creation (for tool-granted sessions)
// and killed when the session ends or on timeout.
//
// Protocol: JSON-RPC 2.0 over stdio. Each message is a single JSON line
// terminated by `\n`.

use crate::tools::ToolGrant;
use lot::{SandboxCommand, SandboxPolicyBuilder, SandboxStdio};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Output from a `NuShell` MCP `evaluate` call.
#[derive(Debug)]
pub struct NuOutput {
    pub content: String,
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// JSON-RPC wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    id: Option<u64>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    message: String,
}

// MCP content block returned by tools/call.
#[derive(Deserialize)]
struct McpContent {
    text: Option<String>,
}

#[derive(Deserialize)]
struct McpToolResult {
    content: Vec<McpContent>,
    #[serde(rename = "isError")]
    is_error: Option<bool>,
}

// ---------------------------------------------------------------------------
// Internal process state
// ---------------------------------------------------------------------------

/// Maximum number of non-matching lines to skip before giving up.
const MAX_SKIPPED_LINES: usize = 64;

/// Shared handle to the child process, accessible for killing from outside
/// the blocking I/O thread.
type ChildHandle = Arc<std::sync::Mutex<Option<lot::SandboxedChild>>>;

/// Shared handle to stdin, closeable from `kill()` to unblock the inner
/// child even if the `NuProcess` is trapped in a `spawn_blocking` closure.
type StdinHandle = Arc<std::sync::Mutex<Option<File>>>;

struct NuProcess {
    stdin: StdinHandle,
    stdout: BufReader<File>,
    next_id: u64,
    /// The grant under which this process was spawned (determines sandbox policy).
    grant: ToolGrant,
    /// Project root the sandbox is anchored to.
    project_root: PathBuf,
    /// Shared handle to the child — kept alive for cleanup, accessible for kill.
    child_handle: ChildHandle,
    /// Per-session temp directory under `<project_root>/.reel/tmp/`.
    /// Dropped (and cleaned up) when the process is dropped.
    _session_temp_dir: tempfile::TempDir,
}

impl Drop for NuProcess {
    fn drop(&mut self) {
        let mut guard = self
            .child_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ref mut child) = *guard {
            let _ = child.kill();
            // Reap the child so it releases handles before _session_temp_dir
            // is dropped — on Windows, open handles prevent directory deletion.
            let _ = child.wait();
        }
    }
}

/// Combined session state behind a single mutex to prevent lock-ordering deadlocks.
#[derive(Default)]
struct SessionState {
    process: Option<NuProcess>,
    generation: u64,
    /// Shared child handle kept here so `kill()` can reach the child even when
    /// the `NuProcess` has been taken out for blocking I/O in `evaluate_inner`.
    inflight_child: Option<ChildHandle>,
    /// Shared stdin handle kept here so `kill()` can close stdin to trigger
    /// EOF on the inner child, causing it to exit even if the lot-level kill
    /// doesn't terminate it. Defense-in-depth for the PID namespace issue.
    inflight_stdin: Option<StdinHandle>,
}

/// Manages a persistent `nu --mcp` process.
///
/// Thread-safe via internal `Mutex`. The process is spawned eagerly via
/// `spawn()` and restarted if the grant or project root changes between calls.
pub struct NuSession {
    state: Mutex<SessionState>,
    /// Cache directory containing nu binary, rg binary, and config files.
    /// Defaults to the build-time `NU_CACHE_DIR`. Tests override this to
    /// isolate sandbox ACL operations per test.
    cache_dir: Option<PathBuf>,
}

/// Write a JSON-RPC message as a single `\n`-terminated line.
fn send_line(stdin: &StdinHandle, payload: &[u8]) -> Result<(), String> {
    let mut guard = stdin
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = guard
        .as_mut()
        .ok_or_else(|| "stdin closed (session killed)".to_string())?;
    (|| -> io::Result<()> {
        sink.write_all(payload)?;
        sink.write_all(b"\n")?;
        sink.flush()
    })()
    .map_err(|e| format!("failed to write to nu stdin: {e}"))
}

impl NuSession {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SessionState::default()),
            cache_dir: option_env!("NU_CACHE_DIR").map(PathBuf::from),
        }
    }

    /// Create a session with an explicit cache directory override.
    ///
    /// Used by tests to isolate each sandbox test's exec_path, avoiding
    /// concurrent AppContainer ACL conflicts on shared directories.
    #[cfg(test)]
    fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self {
            state: Mutex::new(SessionState::default()),
            cache_dir: Some(cache_dir),
        }
    }

    /// Eagerly spawn the nu MCP process so it is warm by the first tool call.
    pub async fn spawn(&self, project_root: &Path, grant: ToolGrant) -> Result<(), String> {
        let mut st = self.state.lock().await;
        if st.process.is_some() {
            return Ok(());
        }
        st.generation += 1;
        let proc = spawn_nu_process(project_root, grant, self.cache_dir.as_deref()).await?;
        st.process = Some(proc);
        // Release the mutex before returning so other callers (evaluate, kill)
        // are not blocked while the caller continues.
        drop(st);
        Ok(())
    }

    /// Execute a `NuShell` command via the MCP `evaluate` tool.
    ///
    /// If the grant or project root differs from the running process, the
    /// old process is killed and a new one is spawned.
    pub async fn evaluate(
        &self,
        command: &str,
        timeout_secs: u64,
        project_root: &Path,
        grant: ToolGrant,
    ) -> Result<NuOutput, String> {
        let timeout = std::time::Duration::from_secs(timeout_secs);

        if let Ok(result) =
            tokio::time::timeout(timeout, self.evaluate_inner(command, project_root, grant)).await
        {
            result
        } else {
            // Timeout: kill the nu process and bump generation so the stale
            // blocking thread cannot write back its process.
            self.kill().await;
            Err(format!(
                "command timed out after {timeout_secs}s — nu session terminated, next call spawns a fresh session"
            ))
        }
    }

    /// Kill the current nu process if one is running.
    ///
    /// Also kills any in-flight child whose `NuProcess` was taken out of state
    /// for blocking I/O — this is what makes timeout-kill work.
    pub async fn kill(&self) {
        let mut st = self.state.lock().await;
        // Bump generation so any in-flight blocking thread won't write back.
        st.generation += 1;

        // Kill the in-flight child first (process taken out during evaluate_inner Phase 2).
        if let Some(ref handle) = st.inflight_child {
            let mut guard = handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut child) = *guard {
                let _ = child.kill();
            }
        }
        st.inflight_child = None;

        // Close stdin so the inner child gets EOF and exits, even if lot's
        // kill didn't terminate it. This also unblocks any spawn_blocking
        // task stuck on read_line (the inner child closes stdout on exit).
        if let Some(ref handle) = st.inflight_stdin {
            let mut guard = handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.take(); // Drop the File, closing the pipe
        }
        st.inflight_stdin = None;

        // Kill the process if it's parked in state (not currently in-flight).
        if let Some(proc) = st.process.take() {
            let mut child_guard = proc
                .child_handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut child) = *child_guard {
                let _ = child.kill();
            }
        }
    }

    async fn evaluate_inner(
        &self,
        command: &str,
        project_root: &Path,
        grant: ToolGrant,
    ) -> Result<NuOutput, String> {
        // Phase 1: Acquire lock, ensure process is running, take it out.
        // Store the child handle in state so kill() can reach it during Phase 2.
        let (proc, generation_at_start) = {
            let mut st = self.state.lock().await;

            let needs_restart = st
                .process
                .as_ref()
                .is_none_or(|p| p.grant != grant || p.project_root != project_root);

            if needs_restart {
                // Bump generation when spawning a new process.
                st.generation += 1;
                st.process.take();
                let new_proc =
                    spawn_nu_process(project_root, grant, self.cache_dir.as_deref()).await?;
                st.process = Some(new_proc);
            }

            let proc = st
                .process
                .take()
                .ok_or("internal: process unavailable after spawn")?;
            st.inflight_child = Some(Arc::clone(&proc.child_handle));
            st.inflight_stdin = Some(Arc::clone(&proc.stdin));
            let generation = st.generation;
            drop(st);
            (proc, generation)
        };
        // Lock released — blocking I/O below does not hold the async mutex,
        // allowing timeout + kill() to work. kill() can reach the child via
        // inflight_child and close stdin via inflight_stdin to unblock the
        // spawn_blocking thread.

        // Phase 2: Blocking I/O on a dedicated thread.
        let command = command.to_owned();
        let child_handle = Arc::clone(&proc.child_handle);
        let mut proc = proc;
        let (proc, result) = tokio::task::spawn_blocking(move || {
            let result = rpc_call(&mut proc, &command);
            (proc, result)
        })
        .await
        .map_err(|e| format!("rpc task panicked: {e}"))?;

        // Phase 3: Put process back only if generation hasn't changed
        // (no kill or respawn happened while we were blocked).
        let mut st = self.state.lock().await;
        st.inflight_child = None;
        st.inflight_stdin = None;
        if result.is_ok() && st.generation == generation_at_start {
            st.process = Some(proc);
        } else if result.is_err() {
            // Kill the process on RPC error to avoid leaking it.
            let mut child_guard = child_handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut child) = *child_guard {
                let _ = child.kill();
            }
            // proc is dropped here, NuProcess::Drop will also attempt kill (idempotent).
        }
        // Release the mutex before returning so timeout/kill callers are not
        // blocked while the result propagates up.
        drop(st);

        result
    }
}

/// Try to parse a line as a JSON-RPC response matching the expected id.
/// Returns `Some(response)` on match, `None` if the line should be skipped.
fn try_parse_response(line: &str, expected_id: u64) -> Option<JsonRpcResponse> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let response: JsonRpcResponse = serde_json::from_str(trimmed).ok()?;
    if response.id != Some(expected_id) {
        return None;
    }
    Some(response)
}

/// Read lines from `reader` until a JSON-RPC response with the given `id` is
/// found. Skips empty lines, malformed JSON, notifications, and responses for
/// other ids, up to `MAX_SKIPPED_LINES`.
fn read_response(
    reader: &mut BufReader<File>,
    expected_id: u64,
) -> Result<JsonRpcResponse, String> {
    let mut skipped = 0usize;
    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|e| format!("failed to read from nu stdout: {e}"))?;

        if bytes_read == 0 {
            return Err("nu process closed stdout unexpectedly".into());
        }

        if let Some(response) = try_parse_response(&line, expected_id) {
            return Ok(response);
        }
        skipped += 1;
        if skipped > MAX_SKIPPED_LINES {
            return Err("too many non-response lines from nu process".into());
        }
    }
}

/// Execute a single JSON-RPC `tools/call` request and read the response.
/// Runs on a blocking thread — all I/O is synchronous.
fn rpc_call(proc: &mut NuProcess, command: &str) -> Result<NuOutput, String> {
    let request_id = proc.next_id;
    proc.next_id += 1;

    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id: request_id,
        method: "tools/call",
        params: Some(serde_json::json!({
            "name": "evaluate",
            "arguments": {
                "input": command
            }
        })),
    };

    let request_bytes =
        serde_json::to_vec(&request).map_err(|e| format!("failed to serialize request: {e}"))?;

    send_line(&proc.stdin, &request_bytes)?;

    let response = read_response(&mut proc.stdout, request_id)?;

    if let Some(err) = response.error {
        return Ok(NuOutput {
            content: err.message,
            is_error: true,
        });
    }

    if let Some(result) = response.result {
        let tool_result: McpToolResult = serde_json::from_value(result)
            .map_err(|e| format!("failed to parse MCP tool result: {e}"))?;

        let text = tool_result
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        let is_error = tool_result.is_error.unwrap_or(false);

        return Ok(NuOutput {
            content: text,
            is_error,
        });
    }

    Err("MCP response had neither result nor error".into())
}

// ---------------------------------------------------------------------------
// Process spawning
// ---------------------------------------------------------------------------

/// Resolve a binary by name using a standard search order:
/// 1. Same directory as the current executable (release packaging).
/// 2. Provided cache directory.
/// 3. Bare name on PATH.
fn resolve_cached_binary(binary_name: &str, cache_dir: Option<&Path>) -> OsString {
    // 1. Next to the current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(binary_name);
            if candidate.exists() {
                return candidate.into_os_string();
            }
        }
    }

    // 2. Cache directory (nu and rg share the same directory).
    if let Some(dir) = cache_dir {
        let candidate = dir.join(binary_name);
        if candidate.exists() {
            return candidate.into_os_string();
        }
    }

    // 3. PATH fallback.
    OsString::from(binary_name)
}

fn resolve_nu_binary(cache_dir: Option<&Path>) -> OsString {
    resolve_cached_binary(if cfg!(windows) { "nu.exe" } else { "nu" }, cache_dir)
}

/// Resolve the path to the `rg` (ripgrep) binary.
///
/// Returns `Some` only when a validated absolute path exists on disk.
/// Used by `spawn_nu_process` to set `REEL_RG_PATH`; the nu-side grep
/// command falls back to bare `rg` when the env var is absent.
pub fn resolve_rg_binary(cache_dir: Option<&Path>) -> Option<PathBuf> {
    let resolved = resolve_cached_binary(if cfg!(windows) { "rg.exe" } else { "rg" }, cache_dir);
    let path = Path::new(&resolved);
    if path.is_absolute() && path.exists() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// Resolve config file paths from the cache directory.
///
/// Returns `(reel_config.nu, reel_env.nu)` as absolute `PathBuf`s, or `None`
/// if the cache directory is unavailable or files don't exist.
fn resolve_config_files(cache_dir: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    let dir = cache_dir?;
    let config = dir.join("reel_config.nu");
    let env = dir.join("reel_env.nu");
    if config.exists() && env.exists() {
        Some((config, env))
    } else {
        None
    }
}

/// Build the sandbox policy for the nu process.
fn build_nu_sandbox_policy(
    project_root: &Path,
    grant: ToolGrant,
    cache_dir: Option<&Path>,
    session_temp_dir: &Path,
) -> lot::Result<lot::SandboxPolicy> {
    let mut builder = SandboxPolicyBuilder::new()
        .write_path(session_temp_dir)
        .allow_network(true);

    if grant.contains(ToolGrant::WRITE) {
        builder = builder.write_path(project_root);
    } else {
        builder = builder.read_path(project_root);
    }

    // Grant exec access to the cache directory so nu can read config files
    // and execute the rg binary from there. exec_path implies read on all
    // platforms (Linux: MS_RDONLY without MS_NOEXEC, macOS: file-read* +
    // process-exec, Windows: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE).
    if let Some(dir) = cache_dir {
        builder = builder.exec_path(dir);
    }

    builder.build()
}

/// Spawn a `nu --mcp` process inside a lot sandbox and perform the MCP
/// initialization handshake. The entire spawn + handshake runs on a blocking
/// thread to avoid blocking the async runtime.
///
/// If reel config files exist in the build cache, passes `--config` and
/// `--env-config` flags so reel custom commands (`reel read`, etc.) are
/// available immediately without an evaluate preamble.
async fn spawn_nu_process(
    project_root: &Path,
    grant: ToolGrant,
    cache_dir: Option<&Path>,
) -> Result<NuProcess, String> {
    // Validate project root exists before creating any directories under it.
    if !project_root.exists() {
        return Err(format!(
            "project root does not exist: {}",
            project_root.display()
        ));
    }

    // Create a per-session temp directory under <project_root>/.reel/tmp/ so
    // that all ancestor directories match those already granted traverse ACEs
    // by the consumer's setup command. This avoids the nu_glob ancestor
    // traversal failures that occur when temp dirs live under system %TEMP%.
    let temp_base = project_root.join(".reel").join("tmp");
    std::fs::create_dir_all(&temp_base)
        .map_err(|e| format!("failed to create session temp base: {e}"))?;
    let session_temp_dir = tempfile::TempDir::new_in(&temp_base)
        .map_err(|e| format!("failed to create session temp dir: {e}"))?;

    let policy = build_nu_sandbox_policy(project_root, grant, cache_dir, session_temp_dir.path())
        .map_err(|e| format!("sandbox setup failed: {e}"))?;

    let nu_binary = resolve_nu_binary(cache_dir);
    let config_files = resolve_config_files(cache_dir);
    let rg_binary = resolve_rg_binary(cache_dir);
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = SandboxCommand::new(&nu_binary);
        cmd.arg("--mcp");

        // Pass reel config files so custom commands are pre-loaded.
        if let Some((ref config_path, ref env_path)) = config_files {
            cmd.arg("--config");
            cmd.arg(config_path);
            cmd.arg("--env-config");
            cmd.arg(env_path);
        }

        cmd.cwd(&project_root);
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Null);
        cmd.stdin(SandboxStdio::Piped);
        // Override TEMP/TMP before forward_common_env — explicit env takes
        // precedence over forwarded values. This redirects nu's temp I/O to
        // the per-session dir under the project root, avoiding ancestor
        // traversal failures in AppContainer.
        cmd.env("TEMP", session_temp_dir.path());
        cmd.env("TMP", session_temp_dir.path());
        cmd.forward_common_env();

        // Set REEL_RG_PATH so reel_config.nu can invoke rg by absolute path,
        // bypassing nu's PATH-based lookup which fails under AppContainer
        // (nu does not split semicolons in PATH list elements for executable search).
        if let Some(ref path) = rg_binary {
            cmd.env("REEL_RG_PATH", path);
        }

        let mut child =
            lot::spawn(&policy, &cmd).map_err(|e| format!("failed to spawn nu: {e}"))?;

        let stdin = child.take_stdin().ok_or("failed to capture nu stdin")?;
        let stdout = child.take_stdout().ok_or("failed to capture nu stdout")?;

        let child_handle: ChildHandle = Arc::new(std::sync::Mutex::new(Some(child)));
        let stdin_handle: StdinHandle = Arc::new(std::sync::Mutex::new(Some(stdin)));

        let mut proc = NuProcess {
            stdin: stdin_handle,
            stdout: BufReader::new(stdout),
            next_id: 1,
            grant,
            project_root,
            child_handle,
            _session_temp_dir: session_temp_dir,
        };

        // MCP initialization handshake.
        let init_request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 0,
            method: "initialize",
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "reel",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        };

        let init_bytes = serde_json::to_vec(&init_request)
            .map_err(|e| format!("failed to serialize init request: {e}"))?;

        send_line(&proc.stdin, &init_bytes)?;

        // Read initialize response (uses skip loop like rpc_call).
        let init_response = read_response(&mut proc.stdout, 0)?;

        if let Some(err) = init_response.error {
            return Err(format!("MCP initialize failed: {}", err.message));
        }

        // Send initialized notification (no id, no response expected).
        let initialized = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });

        let notif_bytes = serde_json::to_vec(&initialized)
            .map_err(|e| format!("failed to serialize notification: {e}"))?;

        send_line(&proc.stdin, &notif_bytes)?;

        Ok(proc)
    })
    .await
    .map_err(|e| format!("spawn task panicked: {e}"))?
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_borrow,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::match_same_arms
)]
mod tests {
    use super::*;

    #[test]
    fn test_build_nu_sandbox_policy_write_grant() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sess_tmp = tempfile::TempDir::new_in(tmp.path()).unwrap();
        let policy = build_nu_sandbox_policy(
            tmp.path(),
            ToolGrant::WRITE | ToolGrant::NU,
            None,
            sess_tmp.path(),
        )
        .unwrap();
        let canon = tmp.path().canonicalize().unwrap();

        let covered_by_write = policy
            .write_paths
            .iter()
            .any(|w| canon.starts_with(w) || w.starts_with(&canon));
        assert!(
            covered_by_write,
            "project root should be writable when WRITE granted"
        );
        assert!(
            !policy.read_paths.contains(&canon),
            "project root should NOT be in read_paths when WRITE granted"
        );
        // Session temp dir writability is tested by the no_write_grant variant,
        // where it's not subsumed by the project root write path.
    }

    #[test]
    fn test_build_nu_sandbox_policy_no_write_grant() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sess_tmp = tempfile::TempDir::new_in(tmp.path()).unwrap();
        let policy =
            build_nu_sandbox_policy(tmp.path(), ToolGrant::NU, None, sess_tmp.path()).unwrap();
        let canon = tmp.path().canonicalize().unwrap();
        let sess_canon = sess_tmp.path().canonicalize().unwrap();

        // Without WRITE grant: project root is read-only, session temp is writable.
        assert!(
            policy.read_paths.contains(&canon),
            "project root should be in read_paths when WRITE not granted"
        );
        assert!(
            !policy.write_paths.contains(&canon),
            "project root should NOT be in write_paths when WRITE not granted"
        );
        let has_sess_write = policy.write_paths.iter().any(|w| sess_canon.starts_with(w));
        assert!(
            has_sess_write,
            "session temp dir should be writable regardless of grant"
        );
    }

    #[test]
    fn test_build_nu_sandbox_policy_allows_network() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sess_tmp = tempfile::TempDir::new_in(tmp.path()).unwrap();
        let policy =
            build_nu_sandbox_policy(tmp.path(), ToolGrant::NU, None, sess_tmp.path()).unwrap();
        assert!(policy.allow_network);
    }

    #[test]
    fn test_build_nu_sandbox_policy_no_exec_paths_without_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sess_tmp = tempfile::TempDir::new_in(tmp.path()).unwrap();
        let policy =
            build_nu_sandbox_policy(tmp.path(), ToolGrant::NU, None, sess_tmp.path()).unwrap();
        assert!(
            policy.exec_paths.is_empty(),
            "exec_paths should be empty when no cache dir provided"
        );
    }

    #[test]
    fn test_build_nu_sandbox_policy_includes_cache_dir_exec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sess_tmp = tempfile::TempDir::new_in(tmp.path()).unwrap();
        // Cache dir outside test project root (tmp) to avoid exec/read overlap in policy.
        let cache = tempfile::TempDir::new_in(sandbox_test_base()).unwrap();
        let policy = build_nu_sandbox_policy(
            tmp.path(),
            ToolGrant::NU,
            Some(cache.path()),
            sess_tmp.path(),
        )
        .unwrap();

        let cache_canon = cache.path().canonicalize().unwrap();
        let has_cache_exec = policy
            .exec_paths
            .iter()
            .any(|p| p == &cache_canon || cache_canon.starts_with(p));
        assert!(
            has_cache_exec,
            "sandbox should grant exec access to provided cache dir"
        );
    }

    #[test]
    fn test_resolve_config_files_exist_when_cache_dir_set() {
        // When NU_CACHE_DIR is set (normal build), config files should exist
        // because build.rs writes them.
        let cache_dir = option_env!("NU_CACHE_DIR").map(Path::new);
        if cache_dir.is_none() {
            return; // Build didn't set NU_CACHE_DIR — skip.
        }
        let result = resolve_config_files(cache_dir);
        assert!(
            result.is_some(),
            "config files should exist in NU_CACHE_DIR after build"
        );
        let (config, env) = result.unwrap();
        assert!(config.exists(), "reel_config.nu should exist");
        assert!(env.exists(), "reel_env.nu should exist");
        assert!(config.is_absolute(), "config path should be absolute");
        assert!(env.is_absolute(), "env path should be absolute");
    }

    #[test]
    fn test_resolve_config_files_none_without_cache() {
        assert!(resolve_config_files(None).is_none());
    }

    // -----------------------------------------------------------------------
    // try_parse_response tests
    // -----------------------------------------------------------------------

    #[test]
    fn try_parse_response_matching_id() {
        let line =
            r#"{"jsonrpc":"2.0","id":42,"result":{"content":[{"type":"text","text":"ok"}]}}"#;
        let resp = try_parse_response(line, 42);
        assert!(resp.is_some());
        assert_eq!(resp.unwrap().id, Some(42));
    }

    #[test]
    fn try_parse_response_wrong_id() {
        let line = r#"{"jsonrpc":"2.0","id":99,"result":{}}"#;
        assert!(try_parse_response(line, 42).is_none());
    }

    #[test]
    fn try_parse_response_no_id_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(try_parse_response(line, 0).is_none());
    }

    #[test]
    fn try_parse_response_empty_line() {
        assert!(try_parse_response("", 1).is_none());
        assert!(try_parse_response("   \n", 1).is_none());
    }

    #[test]
    fn try_parse_response_malformed_json() {
        assert!(try_parse_response("{not json", 1).is_none());
    }

    #[test]
    fn try_parse_response_with_error() {
        let line = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"bad request"}}"#;
        let resp = try_parse_response(line, 1);
        assert!(resp.is_some());
        let resp = resp.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().message, "bad request");
    }

    #[test]
    fn try_parse_response_with_surrounding_whitespace() {
        let line = r#"  {"jsonrpc":"2.0","id":5,"result":{}}  "#;
        let resp = try_parse_response(line, 5);
        assert!(resp.is_some());
    }

    // -----------------------------------------------------------------------
    // read_response tests
    // -----------------------------------------------------------------------

    fn buf_reader_from_str(s: &str) -> BufReader<File> {
        use std::io::{Seek, Write as IoWrite};
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(s.as_bytes()).unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        BufReader::new(file)
    }

    #[test]
    fn read_response_skips_blank_lines() {
        let data = "\n\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
        let mut reader = buf_reader_from_str(data);
        let resp = read_response(&mut reader, 1).unwrap();
        assert_eq!(resp.id, Some(1));
    }

    #[test]
    fn read_response_skips_non_matching_ids() {
        let data = "{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n\
                    {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
        let mut reader = buf_reader_from_str(data);
        let resp = read_response(&mut reader, 1).unwrap();
        assert_eq!(resp.id, Some(1));
    }

    #[test]
    fn read_response_eof_returns_error() {
        let data = "";
        let mut reader = buf_reader_from_str(data);
        let result = read_response(&mut reader, 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("closed stdout"));
    }

    #[test]
    fn read_response_too_many_skipped_lines() {
        // MAX_SKIPPED_LINES + 2 lines of garbage, no matching response.
        let data: String = (0..MAX_SKIPPED_LINES + 2).map(|_| "not json\n").collect();
        let mut reader = buf_reader_from_str(&data);
        let result = read_response(&mut reader, 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many non-response lines"));
    }

    #[test]
    fn read_response_skips_notifications() {
        let data = "{\"jsonrpc\":\"2.0\",\"method\":\"log\",\"params\":{}}\n\
                    {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n";
        let mut reader = buf_reader_from_str(data);
        let resp = read_response(&mut reader, 3).unwrap();
        assert_eq!(resp.id, Some(3));
    }

    // -----------------------------------------------------------------------
    // Generation-based session invalidation tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn session_new_starts_with_no_process() {
        let session = NuSession::new();
        let st = session.state.lock().await;
        assert!(st.process.is_none());
        assert_eq!(st.generation, 0);
    }

    #[tokio::test]
    async fn kill_increments_generation() {
        let session = NuSession::new();
        {
            let st = session.state.lock().await;
            assert_eq!(st.generation, 0);
        }
        session.kill().await;
        {
            let st = session.state.lock().await;
            assert_eq!(st.generation, 1);
        }
        session.kill().await;
        {
            let st = session.state.lock().await;
            assert_eq!(st.generation, 2);
        }
    }

    #[tokio::test]
    async fn kill_on_empty_session_is_safe() {
        let session = NuSession::new();
        // Calling kill with no process should not panic.
        session.kill().await;
        session.kill().await;
    }

    // -----------------------------------------------------------------------
    // Integration tests — spawn real nu processes
    // -----------------------------------------------------------------------

    /// Returns true if the nu binary is resolvable. Tests that need a real
    /// nu process should call this and return early if false.
    fn nu_available() -> bool {
        let nu = resolve_nu_binary(option_env!("NU_CACHE_DIR").map(Path::new));
        let path = Path::new(&nu);
        // If resolve returned an absolute path, check existence directly.
        if path.is_absolute() {
            return path.exists();
        }
        // Bare name: try running it.
        std::process::Command::new(&nu)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    macro_rules! skip_no_nu {
        () => {
            if !nu_available() {
                eprintln!("SKIP: nu binary not available");
                return;
            }
        };
    }

    fn tmp_project() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("create temp dir")
    }

    /// Base directory for sandbox test temp dirs.
    fn sandbox_test_base() -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("sandbox-test");
        std::fs::create_dir_all(&base).expect("create sandbox test base dir");
        base
    }

    /// Create a temp project directory under `sandbox_test_base()` for sandbox tests.
    fn tmp_sandbox_project() -> tempfile::TempDir {
        tempfile::TempDir::new_in(sandbox_test_base()).expect("create sandbox test dir")
    }

    /// Create an isolated copy of the build-time nu-cache directory.
    ///
    /// Each sandbox test gets its own cache dir so AppContainer ACL
    /// operations on exec_path do not interfere between concurrent tests.
    fn tmp_sandbox_cache() -> Option<tempfile::TempDir> {
        let src = option_env!("NU_CACHE_DIR")?;
        let dest =
            tempfile::TempDir::new_in(sandbox_test_base()).expect("create sandbox cache dir");
        for entry in std::fs::read_dir(src).expect("read cache dir") {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            if path.is_file() {
                std::fs::copy(&path, dest.path().join(path.file_name().unwrap()))
                    .expect("copy cache file");
            }
        }
        Some(dest)
    }

    /// Create a NuSession with an isolated copy of the build-time cache dir.
    ///
    /// Each test gets its own cache dir so concurrent AppContainer profiles
    /// do not interfere via ACL grant/restore on a shared directory.
    /// The returned `Option<TempDir>` must be held alive for the test duration.
    fn isolated_session() -> (NuSession, Option<tempfile::TempDir>) {
        let cache = tmp_sandbox_cache();
        let session = match &cache {
            Some(c) => NuSession::with_cache_dir(c.path().to_path_buf()),
            None => NuSession::new(),
        };
        (session, cache)
    }

    /// Sandbox test environment with isolated project and cache directories.
    /// Field order matters: Rust drops fields in declaration order.
    /// `session` must drop first so the nu process is killed before
    /// the TempDirs try to delete nu.exe / rg.exe on Windows.
    struct SandboxTestEnv {
        session: NuSession,
        project: tempfile::TempDir,
        _cache: Option<tempfile::TempDir>,
    }

    fn sandbox_env() -> SandboxTestEnv {
        let project = tmp_sandbox_project();
        let (session, cache) = isolated_session();
        SandboxTestEnv {
            project,
            _cache: cache,
            session,
        }
    }

    /// Format a path for use in nu commands (forward slashes).
    fn nu_path(p: &Path) -> String {
        p.to_str().unwrap().replace('\\', "/")
    }

    /// Spawn a session, panicking if sandbox setup fails.
    async fn try_spawn(session: &NuSession, root: &Path, grant: ToolGrant) {
        session
            .spawn(root, grant)
            .await
            .expect("spawn should succeed (sandbox setup failure is fatal)");
    }

    /// Evaluate a command, panicking if sandbox setup fails.
    async fn try_eval(
        session: &NuSession,
        cmd: &str,
        timeout: u64,
        root: &Path,
        grant: ToolGrant,
    ) -> Result<NuOutput, String> {
        let result = session.evaluate(cmd, timeout, root, grant).await;
        if let Err(e) = &result {
            assert!(
                !e.contains("sandbox setup failed"),
                "sandbox setup failed (this is fatal): {e}"
            );
        }
        result
    }

    #[tokio::test]
    async fn integration_spawn_creates_session() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        try_spawn(&session, tmp.path(), ToolGrant::NU).await;
    }

    #[tokio::test]
    async fn integration_spawn_is_idempotent() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        try_spawn(&session, tmp.path(), ToolGrant::NU).await;
        // Second spawn with same params is a no-op.
        session.spawn(tmp.path(), ToolGrant::NU).await.unwrap();
    }

    #[tokio::test]
    async fn integration_drop_cleans_up() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        {
            let (session, _cache) = isolated_session();
            try_spawn(&session, tmp.path(), ToolGrant::NU).await;
        }
        // No panic or zombie = pass.
    }

    #[tokio::test]
    async fn integration_kill_then_evaluate_respawns() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        try_spawn(&session, tmp.path(), ToolGrant::NU).await;
        session.kill().await;
        let result = try_eval(&session, "echo 'alive'", 30, tmp.path(), ToolGrant::NU).await;
        let out = result.unwrap();
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn integration_evaluate_simple_echo() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        let result = try_eval(
            &session,
            "echo 'hello world'",
            30,
            tmp.path(),
            ToolGrant::NU,
        )
        .await;
        let out = result.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("hello world"));
    }

    #[tokio::test]
    async fn integration_evaluate_error_command() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        let result = try_eval(
            &session,
            "error make { msg: 'test error' }",
            30,
            tmp.path(),
            ToolGrant::NU,
        )
        .await;
        let out = result.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("test error"));
    }

    #[tokio::test]
    async fn integration_evaluate_multiple_sequential() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        let result = try_eval(&session, "1 + 2", 30, tmp.path(), ToolGrant::NU).await;
        let out1 = result.unwrap();
        assert!(!out1.is_error);
        assert!(out1.content.contains('3'));
        let out2 = session
            .evaluate("'foo' | str length", 30, tmp.path(), ToolGrant::NU)
            .await
            .unwrap();
        assert!(!out2.is_error);
        assert!(out2.content.contains('3'));
    }

    #[tokio::test]
    async fn integration_custom_command_reel_read() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "line one\nline two\n").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;
        let cmd = format!("reel read '{}'", nu_path(&test_file));
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(!out.is_error, "reel read failed: {}", out.content);
        assert!(out.content.contains("line one"));
    }

    #[tokio::test]
    async fn integration_custom_command_reel_write() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("written.txt");
        let grant = ToolGrant::NU | ToolGrant::WRITE;
        let cmd = format!("reel write '{}' 'hello from test'", nu_path(&test_file));
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(!out.is_error, "reel write failed: {}", out.content);
        let content = std::fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "hello from test");
    }

    #[tokio::test]
    async fn integration_custom_command_reel_glob() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;
        let cmd = format!("reel glob '*.txt' --path '{}'", nu_path(tmp.path()));
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(!out.is_error, "reel glob failed: {}", out.content);
        assert!(out.content.contains("a.txt"));
        assert!(out.content.contains("b.txt"));
    }

    #[tokio::test]
    async fn integration_custom_command_reel_edit() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("edit_me.txt");
        std::fs::write(&test_file, "old value here").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;
        let cmd = format!(
            "reel edit '{}' 'old value' 'new value'",
            nu_path(&test_file)
        );
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(!out.is_error, "reel edit failed: {}", out.content);
        let content = std::fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "new value here");
    }

    // === DIAGNOSTIC TESTS: probing raw nu commands inside AppContainer ===

    /// Test raw `ls` on a file inside sandbox — does it fail like `reel read`?
    #[tokio::test]
    async fn diag_raw_ls_in_sandbox() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "hello").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // Test 1: raw ls with path
        let cmd = format!("ls '{}' | to json", nu_path(&test_file));
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG raw ls result: {:?}", result);

        // Test 2: ls via path expand (same as reel read does)
        let cmd2 = format!(
            "let full = ('{}' | path expand); ls $full | to json",
            nu_path(&test_file)
        );
        let result2 = try_eval(session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-via-path-expand result: {:?}", result2);

        // Test 3: just path expand alone — what does it return?
        let cmd3 = format!("'{}' | path expand", nu_path(&test_file));
        let result3 = try_eval(session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG path-expand result: {:?}", result3);

        // Test 4: ls the directory instead of the file
        let cmd4 = format!("ls '{}' | to json", nu_path(tmp.path()));
        let result4 = try_eval(session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-dir result: {:?}", result4);

        // Test 5: open raw (same as reel edit does)
        let cmd5 = format!("open '{}' --raw", nu_path(&test_file));
        let result5 = try_eval(session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-raw result: {:?}", result5);

        // Test 6: open with path expand (what reel edit actually does)
        let cmd6 = format!(
            "let full = ('{}' | path expand); open $full --raw",
            nu_path(&test_file)
        );
        let result6 = try_eval(session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-via-path-expand result: {:?}", result6);
    }

    /// Test what path expand actually produces inside the sandbox
    #[tokio::test]
    async fn diag_path_expand_in_sandbox() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "hello").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // What does nu see as the path?
        let cmd = format!("'{}' | path expand | to json", nu_path(&test_file));
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG path-expand-json: {:?}", result);

        // What is pwd inside the sandbox?
        let cmd2 = "pwd";
        let result2 = try_eval(session, cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG pwd: {:?}", result2);

        // Does `ls` work at all with a simple relative file?
        let cmd3 = "ls test.txt | to json";
        let result3 = try_eval(session, cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-relative: {:?}", result3);

        // Does `open` work with a relative file?
        let cmd4 = "open test.txt --raw";
        let result4 = try_eval(session, cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-relative: {:?}", result4);
    }

    /// Test mkdir behavior (for reel write failure: "Already exists")
    #[tokio::test]
    async fn diag_mkdir_in_sandbox() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("written.txt");
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // What does path dirname return?
        let cmd = format!(
            "let full = ('{}' | path expand); $full | path dirname",
            nu_path(&test_file)
        );
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG path-dirname: {:?}", result);

        // Does mkdir fail on an existing dir?
        let cmd2 = format!("mkdir '{}'", nu_path(tmp.path()));
        let result2 = try_eval(session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG mkdir-existing: {:?}", result2);

        // Does mkdir via path expand fail?
        let cmd3 = format!(
            "let full = ('{}' | path expand); let parent = ($full | path dirname); mkdir $parent",
            nu_path(&test_file)
        );
        let result3 = try_eval(session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG mkdir-via-expand: {:?}", result3);

        // What about save directly without mkdir?
        let cmd4 = format!("'hello' | save --force '{}'", nu_path(&test_file));
        let result4 = try_eval(session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG save-direct: {:?}", result4);
    }

    /// Same commands as diag_raw_ls_in_sandbox but WITHOUT AppContainer.
    /// Uses tmp_project() instead of sandbox_env() to get a non-sandboxed session.
    #[tokio::test]
    async fn diag_raw_ls_no_sandbox() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "hello").unwrap();
        let grant = ToolGrant::NU; // no WRITE, but doesn't matter for ls/open

        // ls file
        let cmd = format!("ls '{}' | to json", nu_path(&test_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG-NOSANDBOX ls-file: {:?}", result);

        // ls dir
        let cmd2 = format!("ls '{}' | to json", nu_path(tmp.path()));
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG-NOSANDBOX ls-dir: {:?}", result2);

        // open raw
        let cmd3 = format!("open '{}' --raw", nu_path(&test_file));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG-NOSANDBOX open-raw: {:?}", result3);

        // mkdir existing dir
        let cmd4 = format!("mkdir '{}'", nu_path(tmp.path()));
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG-NOSANDBOX mkdir-existing: {:?}", result4);

        // ls relative
        let cmd5 = "ls test.txt | to json";
        let result5 = try_eval(&session, cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG-NOSANDBOX ls-relative: {:?}", result5);
    }

    /// Test using a file in a KNOWN NON-TEMP directory (the project root itself)
    /// to determine if the issue is temp-dir-path related.
    #[tokio::test]
    async fn diag_ls_on_known_file() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        // Use a file in the cargo manifest dir (known to exist, not temp-based)
        let known_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let grant = ToolGrant::NU;

        let cmd = format!("ls '{}' | to json", nu_path(&known_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-known-file: {:?}", result);

        let cmd2 = format!(
            "open '{}' --raw | str substring 0..50",
            nu_path(&known_file)
        );
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-known-file: {:?}", result2);
    }

    /// Test the same file created BEFORE nu starts vs AFTER (via nu itself).
    /// This checks if the issue is about file visibility at nu startup time.
    #[tokio::test]
    async fn diag_file_created_by_nu() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // First, create a file VIA nu
        let test_file = tmp.path().join("nu_created.txt");
        let cmd = format!("'hello nu' | save '{}' --force", nu_path(&test_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG save-by-nu: {:?}", result);

        // Now try to ls and open the file nu just created
        let cmd2 = format!("ls '{}' | to json", nu_path(&test_file));
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-nu-created: {:?}", result2);

        let cmd3 = format!("open '{}' --raw", nu_path(&test_file));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-nu-created: {:?}", result3);
    }

    /// Probe MCP-specific behavior: is the issue in how nu MCP processes
    /// file operations vs. how non-MCP nu does?
    #[tokio::test]
    async fn diag_mcp_file_ops() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        let test_file = tmp.path().join("probe.txt");
        std::fs::write(&test_file, "probe content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // Does glob find the file?
        let cmd = format!("glob '{}' | to json", nu_path(&test_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG glob-exact-file: {:?}", result);

        // Does ls with glob pattern work? (*.txt instead of exact filename)
        let cmd2 = format!("ls (glob '{}' | first)", nu_path(&test_file));
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-via-glob: {:?}", result2);

        // ls *.txt in the directory
        let cmd3 = format!("cd '{}'; ls *.txt | to json", nu_path(tmp.path()));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-glob-pattern: {:?}", result3);

        // Does `^ls` (external ls) work?
        // Note: on Windows this would be `cmd /c dir` — skip if not available
        // Try powershell Test-Path
        let cmd4 = format!("^powershell -c \"Test-Path '{}'\"", test_file.display());
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG powershell-test-path: {:?}", result4);

        // Does `path exists` work?
        let cmd5 = format!("'{}' | path exists", nu_path(&test_file));
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG path-exists: {:?}", result5);

        // Does `^cat` (external) work?
        let cmd6 = format!("^cmd /c type '{}'", test_file.display());
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG external-cat: {:?}", result6);
    }

    /// Probe whether the issue is in MCP response serialization vs command execution
    #[tokio::test]
    async fn diag_mcp_serialization() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        let test_file = tmp.path().join("probe.txt");
        std::fs::write(&test_file, "probe content 123").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // Read file using alternative methods that don't use `open`:
        // 1. Using `cat` — does nu have this?
        // 2. Using sys commands
        // 3. Try `open` and pipe through type inspection

        // What type does `open --raw` return?
        let cmd = format!("open '{}' --raw | describe", nu_path(&test_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-describe: {:?}", result);

        // Try piping through `to text`
        let cmd2 = format!("open '{}' --raw | to text", nu_path(&test_file));
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-to-text: {:?}", result2);

        // Try `open` without --raw
        let cmd3 = format!("open '{}'", nu_path(&test_file));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-no-raw: {:?}", result3);

        // Alternative file read: use `^powershell Get-Content` or just test what `open` really returns
        // Actually: can we read the file line by line?
        let cmd4 = format!("open '{}' --raw | lines | length", nu_path(&test_file));
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-lines-length: {:?}", result4);

        // Use `open --raw | bytes length` to see if binary data is there
        let cmd5 = format!("open '{}' --raw | bytes length", nu_path(&test_file));
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-bytes-length: {:?}", result5);

        // Check if `open` produces binary that MCP can't serialize
        let cmd6 = format!(
            "open '{}' --raw | encode utf-8 | decode utf-8",
            nu_path(&test_file)
        );
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-encode-decode: {:?}", result6);
    }

    /// Check what MCP tools nu exposes and probe filesystem access patterns
    #[tokio::test]
    async fn diag_mcp_tools_list() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        let test_file = tmp.path().join("probe.txt");
        std::fs::write(&test_file, "hello mcp").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // First, spawn the session to get the nu process running
        try_spawn(&session, tmp.path(), grant).await;

        // Send a tools/list request to see what tools nu MCP exposes
        // We need to use the raw RPC — let me use evaluate to test instead.

        // Let's try: does `do { open $file }` work differently?
        let cmd = format!("do {{ open '{}' --raw }}", nu_path(&test_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG do-block-open: {:?}", result);

        // Let's test: what does `open` see when we give it the canonical path?
        let cmd2 = format!(
            "let p = ('{}' | path expand); $p | path exists",
            nu_path(&test_file)
        );
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG expand-then-exists: {:?}", result2);

        // Check if the issue is `open` treating the result as a URL or something
        // Does `open --raw` on a directory path work?
        let cmd3 = format!("open '{}' --raw", nu_path(tmp.path()));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-dir: {:?}", result3);

        // Try reading with `bytes` and `encode` — maybe `open` returns binary
        // that MCP drops silently
        // Actually, let's try to use a different approach entirely:
        // Read the file using sys/process commands
        let cmd4 = "sys host | to json";
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG sys-host: {:?}", result4);

        // Most importantly: can we verify if open even TRIES to access the file?
        // Let's try to `open` a definitely-nonexistent file and compare the error
        let cmd5 = "open '/definitely/nonexistent/file.txt' --raw";
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-nonexistent: {:?}", result5);

        // Now compare: open an existing file (should return content, but returns nothing)
        let cmd6 = format!("open '{}' --raw", nu_path(&test_file));
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-existing: {:?}", result6);
    }

    /// Test if the issue is byte-stream vs string serialization in MCP
    #[tokio::test]
    async fn diag_bytestream_vs_string() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        let test_file = tmp.path().join("probe.txt");
        std::fs::write(&test_file, "hello mcp content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // What does nu return for inline string vs open?
        // 1. Inline string (should work)
        let cmd = "'inline string test'";
        let result = try_eval(&session, cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG inline-string: {:?}", result);

        // 2. Byte literal
        let cmd2 = "0x[68 65 6c 6c 6f]"; // "hello" in hex
        let result2 = try_eval(&session, cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG byte-literal: {:?}", result2);

        // 3. open --raw returns byte stream; can we convert to string first?
        let cmd3 = format!("open '{}' --raw | into string", nu_path(&test_file));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-into-string: {:?}", result3);

        // 4. What about reading file content as lines (not raw)?
        let cmd4 = format!("open '{}' | to text", nu_path(&test_file));
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-text-mode: {:?}", result4);

        // 5. Create a .json file — open without --raw should parse it
        let json_file = tmp.path().join("data.json");
        std::fs::write(&json_file, r#"{"key": "value"}"#).unwrap();
        let cmd5 = format!("open '{}' | to json", nu_path(&json_file));
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-json: {:?}", result5);

        // 6. Create a .csv file
        let csv_file = tmp.path().join("data.csv");
        std::fs::write(&csv_file, "name,age\nalice,30\nbob,25\n").unwrap();
        let cmd6 = format!("open '{}' | to json", nu_path(&csv_file));
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-csv: {:?}", result6);

        // 7. What about a .nu file? (text extension that nu should handle)
        let nu_file = tmp.path().join("test.nu");
        std::fs::write(&nu_file, "echo hello").unwrap();
        let cmd7 = format!("open '{}' --raw | into string", nu_path(&nu_file));
        let result7 = try_eval(&session, &cmd7, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-nu-file: {:?}", result7);
    }

    /// Deep probe: is open truly broken or is something about MCP evaluate
    /// causing the command to not actually run?
    #[tokio::test]
    async fn diag_open_vs_alternatives() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        let test_file = tmp.path().join("probe.txt");
        std::fs::write(&test_file, "hello mcp 12345").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // Alternative 1: use `from raw` or read via glob + open
        // Actually, use the `http` command... no. Let's try:

        // Does `open` work if we force the type?
        let cmd = format!("open '{}' --raw | collect", nu_path(&test_file));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-collect: {:?}", result);

        // Try `cat` alias — does nu have it?
        // Actually, try: print the file content
        let cmd2 = format!(
            "let content = (open '{}' --raw); print $content; 'done'",
            nu_path(&test_file)
        );
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-print: {:?}", result2);

        // Try a completely different approach: read bytes directly
        let cmd3 = format!("open '{}' --raw | bytes length", nu_path(&test_file));
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-raw-bytes-len: {:?}", result3);

        // Does `stor` or `stor open` exist? Let's try something else:
        // Read via shell expansion
        let cmd4 = format!(
            "let p = '{}'; ^type $p",
            nu_path(&test_file).replace('/', "\\")
        );
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG type-command: {:?}", result4);

        // Try: echo the result of open to see if MCP is eating it
        let cmd5 = format!("let x = (open '{}' --raw); $x == null", nu_path(&test_file));
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-eq-null: {:?}", result5);

        // Is the byte stream being consumed by MCP before the pipeline?
        // Test: store in variable, then check type
        let cmd6 = format!(
            "let x = (open '{}' --raw); $x | describe",
            nu_path(&test_file)
        );
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-var-describe: {:?}", result6);
    }

    /// Probe ls behavior more carefully
    #[tokio::test]
    async fn diag_ls_behavior() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        std::fs::write(tmp.path().join("a.txt"), "aaa").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "bbb").unwrap();
        std::fs::create_dir_all(tmp.path().join("subdir")).unwrap();
        std::fs::write(tmp.path().join("subdir").join("c.txt"), "ccc").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // ls with no args (list cwd)
        let cmd = "ls | to json";
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-no-args: {:?}", result);

        // ls *.txt (glob pattern)
        let cmd2 = "ls *.txt | to json";
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-glob-star: {:?}", result2);

        // ls with ** recursive
        let cmd3 = "ls **/*.txt | to json";
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-glob-recursive: {:?}", result3);

        // ls on subdir
        let cmd4 = "ls subdir/ | to json";
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-subdir: {:?}", result4);

        // ls on specific file via relative path
        let cmd5 = "ls a.txt | to json";
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-specific-file: {:?}", result5);
    }

    /// Test if the issue is MCP-specific or also happens with `nu -c` via pipe
    /// Also test the `ls` quirk: why can it list dir contents but not glob?
    #[tokio::test]
    async fn diag_ls_dir_vs_glob() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        std::fs::write(tmp.path().join("test.txt"), "content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // ls with absolute dir path (should work)
        let cmd = format!("ls '{}' | length", nu_path(tmp.path()));
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-absdir-length: {:?}", result);

        // What about `ls .` ?
        let cmd2 = "ls . | length";
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-dot-length: {:?}", result2);

        // What about `ls ./` ?
        let cmd3 = "ls ./ | length";
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-dotslash-length: {:?}", result3);

        // Dir listing works — can we use it to find files?
        let cmd4 = format!(
            "ls '{}' | where name ends-with 'test.txt' | to json",
            nu_path(tmp.path())
        );
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-dir-where: {:?}", result4);

        // Check: does `ls` list the .reel directory? Is the .reel directory the ONLY thing
        // it sees, or does it also see test.txt?
        let cmd5 = format!("ls '{}' | get name | to json", nu_path(tmp.path()));
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-dir-names: {:?}", result5);

        // Test: ls on dir WITHOUT .reel subdir
        let sub = tmp.path().join("clean");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("x.txt"), "xxx").unwrap();
        let cmd6 = format!("ls '{}' | get name | to json", nu_path(&sub));
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-clean-dir: {:?}", result6);
    }

    /// Final probe: why does `ls` (no args) fail but `ls .` works?
    /// And why does `open` return nothing?
    #[tokio::test]
    async fn diag_final_probe() {
        skip_no_nu!();
        let tmp = tmp_project();
        let (session, _cache) = isolated_session();
        std::fs::write(tmp.path().join("test.txt"), "final probe content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // Verify: ls (no args) fails
        let cmd = "ls | length";
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-noargs: {:?}", result);

        // ls . works
        let cmd2 = "ls . | length";
        let result2 = try_eval(&session, &cmd2, 30, tmp.path(), grant).await;
        eprintln!("DIAG ls-dot: {:?}", result2);

        // Nu glob (the command) works for finding files
        let cmd3 = "glob '*' | length";
        let result3 = try_eval(&session, &cmd3, 30, tmp.path(), grant).await;
        eprintln!("DIAG glob-star: {:?}", result3);

        // The critical question for `open`: can we read file content AT ALL in MCP?
        // Try: read file as bytes, then convert
        // Actually: let's try the `^type` windows command (it's `type` in cmd.exe)
        // Or better: just verify that `open` is the problem, not MCP serialization
        // by trying a different read approach

        // Approach: write content, glob the file, then try `open`
        let cmd4 = "'new content' | save 'written_in_mcp.txt' --force; glob 'written_in_mcp.txt'";
        let result4 = try_eval(&session, &cmd4, 30, tmp.path(), grant).await;
        eprintln!("DIAG write-then-glob: {:?}", result4);

        let cmd5 = "open 'written_in_mcp.txt' --raw | describe";
        let result5 = try_eval(&session, &cmd5, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-written-describe: {:?}", result5);

        // Try: does `scope modules` or any nu introspection work?
        // Actually, the most useful test: use nu's built-in `http get` on a file:// URL
        // Or: try `source` command
        // Actually: is the issue in how nu MCP handles byte streams?
        // The MCP evaluate tool likely uses `Value::to_string()` or similar.
        // Let me check if nu has a `str` command that can read files

        // Try: use `lines` command directly (it should work differently from `open`)
        // Actually `lines` needs input. Let's try:
        let cmd6 = format!(
            "'{}' | path expand | open $in --raw | describe",
            nu_path(&tmp.path().join("test.txt"))
        );
        let result6 = try_eval(&session, &cmd6, 30, tmp.path(), grant).await;
        eprintln!("DIAG pipe-open: {:?}", result6);

        // Test: what if we use `open` with explicit type parsing?
        let cmd7 = "open 'test.txt' --raw | decode utf-8";
        let result7 = try_eval(&session, &cmd7, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-decode: {:?}", result7);

        // Hypothesis: MCP evaluate converts ByteStream to nothing during serialization.
        // If true, `open` works but the result gets lost at the MCP boundary.
        // Test: convert to string inside nu before MCP sees it
        let cmd8 = "(open 'test.txt' --raw | decode utf-8)";
        let result8 = try_eval(&session, &cmd8, 30, tmp.path(), grant).await;
        eprintln!("DIAG open-decode-parens: {:?}", result8);
    }

    /// Test nu --mcp WITHOUT lot sandbox — spawn nu directly via std::process::Command.
    /// This determines whether the failures are caused by lot/AppContainer or by nu --mcp itself.
    #[tokio::test]
    async fn diag_nu_mcp_without_lot() {
        skip_no_nu!();
        let cache = tmp_sandbox_cache();
        let cache_dir = cache.as_ref().map(|c| c.path());
        let nu_binary = resolve_nu_binary(cache_dir);
        let config_files = resolve_config_files(cache_dir);

        let tmp = tmp_project();
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "hello no-lot test").unwrap();

        // Spawn nu --mcp directly via std::process::Command (NO lot)
        let mut cmd = std::process::Command::new(&nu_binary);
        cmd.arg("--mcp");
        if let Some((ref config_path, ref env_path)) = config_files {
            cmd.arg("--config").arg(config_path);
            cmd.arg("--env-config").arg(env_path);
        }
        cmd.current_dir(tmp.path());
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());

        let mut child = cmd.spawn().expect("spawn nu --mcp without lot");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());

        // Helper: send JSON-RPC and read response
        fn send_recv(
            stdin: &mut std::process::ChildStdin,
            stdout: &mut BufReader<std::process::ChildStdout>,
            id: u64,
            method: &str,
            params: serde_json::Value,
        ) -> String {
            use std::io::Write;
            let req = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            });
            let bytes = serde_json::to_vec(&req).unwrap();
            stdin.write_all(&bytes).unwrap();
            stdin.write_all(b"\n").unwrap();
            stdin.flush().unwrap();

            // Read lines until we find our response
            loop {
                let mut line = String::new();
                stdout.read_line(&mut line).unwrap();
                if line.is_empty() {
                    return "EOF".to_string();
                }
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
                    if parsed.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        return line.to_string();
                    }
                    // Skip notifications (no id or different id)
                }
            }
        }

        // MCP initialize
        let _init = send_recv(
            &mut stdin,
            &mut stdout,
            0,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "diag", "version": "0.1" }
            }),
        );
        // Send initialized notification (required by MCP protocol)
        {
            use std::io::Write;
            let notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            let bytes = serde_json::to_vec(&notif).unwrap();
            stdin.write_all(&bytes).unwrap();
            stdin.write_all(b"\n").unwrap();
            stdin.flush().unwrap();
        }
        eprintln!("DIAG-NOLOT: MCP init done (with initialized notification)");

        let file_path = nu_path(&test_file);
        let dir_path = nu_path(tmp.path());

        let tests = vec![
            (
                "open --raw | describe",
                format!("open '{}' --raw | describe", file_path),
            ),
            ("ls file", format!("ls '{}'", file_path)),
            ("path exists", format!("'{}' | path exists", file_path)),
            ("ls dir | length", format!("ls '{}' | length", dir_path)),
            ("mkdir existing", format!("mkdir '{}'", dir_path)),
            ("glob file", format!("glob '{}'", file_path)),
            ("ls (no args)", "ls".to_string()),
            ("ls .", "ls . | length".to_string()),
            (
                "open raw null",
                format!("let x = (open '{}' --raw); $x == null", file_path),
            ),
        ];

        for (i, (label, cmd_str)) in tests.iter().enumerate() {
            let resp = send_recv(
                &mut stdin,
                &mut stdout,
                (i + 1) as u64,
                "tools/call",
                serde_json::json!({
                    "name": "evaluate",
                    "arguments": { "input": cmd_str }
                }),
            );
            // Extract just the relevant part
            let truncated = if resp.len() > 300 {
                format!("{}...", &resp[..300])
            } else {
                resp
            };
            eprintln!("DIAG-NOLOT {}: {}", label, truncated);
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    /// Compare lot-spawned vs direct-spawned behavior for the same commands.
    /// This isolates what lot/AppContainer does differently.
    #[tokio::test]
    async fn diag_lot_vs_direct_comparison() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let test_file = tmp.path().join("compare.txt");
        std::fs::write(&test_file, "compare content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let file_path = nu_path(&test_file);
        let dir_path = nu_path(tmp.path());

        // Test detailed filesystem access patterns inside AppContainer
        let tests = vec![
            // Does nu see file metadata at all?
            ("path type", format!("'{}' | path type", file_path)),
            // Does stat work? (if it exists)
            (
                "describe ls-dir entry",
                format!("ls '{}' | first | describe", dir_path),
            ),
            // Can nu read file attributes?
            (
                "path parse",
                format!("'{}' | path parse | to json", file_path),
            ),
            // Can nu use `do -i` to suppress errors?
            (
                "do -i ls file",
                format!("do -i {{ ls '{}' }} | describe", file_path),
            ),
            // What error type does ls produce?
            (
                "try ls file",
                format!(
                    "try {{ ls '{}' | to json }} catch {{ |e| $e | to json }}",
                    file_path
                ),
            ),
            // What about `ls -l`?
            ("ls -la dir", format!("ls -la '{}' | to json", dir_path)),
            // Does `ls` work with a full-form flag?
            (
                "ls --full-paths file",
                format!("ls --full-paths '{}'", file_path),
            ),
            // Does `ls -D` (no symlinks) help?
            ("ls -D file", format!("ls -D '{}'", file_path)),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 300 {
                        format!("{}...", &out.content[..300])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-LOT {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-LOT {}: ERR: {}", label, e),
            }
        }
    }

    /// Probe what nu_glob sees: compare glob patterns vs ls patterns in AppContainer
    #[tokio::test]
    async fn diag_glob_vs_ls_appcontainer() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("a.txt"), "aaa").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "bbb").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let tests = vec![
            // glob works for these patterns
            ("glob *", "glob '*'".to_string()),
            ("glob *.txt", "glob '*.txt'".to_string()),
            ("glob a.txt", "glob 'a.txt'".to_string()),
            // ls fails for the same patterns
            ("ls (no args)", "ls".to_string()),
            ("ls *.txt", "ls *.txt".to_string()),
            ("ls a.txt", "ls a.txt".to_string()),
            // But ls dir works — what if we ls . ?
            ("ls .", "ls . | length".to_string()),
            // nu_glob uses different crate from nu's built-in ls glob
            // Let's try to find the actual filesystem error by checking Win32 APIs
            // Does std::fs::metadata work? (via nu)
            // Actually just check if `ls` is using a different CWD
            ("pwd", "pwd".to_string()),
            // Check if ls uses an internal CWD different from shell CWD
            ("$env.PWD", "$env.PWD".to_string()),
            // Check all env vars related to paths
            ("env TEMP", "$env.TEMP".to_string()),
            ("env TMP", "$env.TMP".to_string()),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 200 {
                        format!("{}...", &out.content[..200])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-GLOB {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-GLOB {}: ERR: {}", label, e),
            }
        }
    }

    /// Test std::fs APIs directly inside AppContainer to see which ones fail.
    /// Run filesystem operations from the test process on files inside AppContainer.
    /// Actually — we need to test from INSIDE the sandboxed nu process.
    /// Use nu commands that map to specific std::fs calls.
    #[tokio::test]
    async fn diag_fs_apis_in_appcontainer() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("test.txt"), "test content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let file_path = nu_path(&tmp.path().join("test.txt"));

        // These test different underlying Win32/Rust APIs:
        let tests = vec![
            // path exists → std::path::Path::exists → GetFileAttributesW
            ("path exists", format!("'{}' | path exists", file_path)),
            // path type → same as exists basically
            ("path type", format!("'{}' | path type", file_path)),
            // glob → wax crate → FindFirstFileW/FindNextFileW
            ("glob exact", format!("glob '{}'", file_path)),
            // ls file → nu_glob → likely GetFileAttributesW + FindFirstFileW
            ("ls file", format!("ls '{}'", file_path)),
            // open → std::fs::File::open → CreateFileW
            ("open", format!("open '{}' --raw | describe", file_path)),
            // ls dir → std::fs::read_dir → FindFirstFileW on dir\*
            ("ls dir", format!("ls '{}' | length", nu_path(tmp.path()))),
            // What about using `ls` on a globbed result piped in?
            // Actually let's try to find what specific Win32 call fails.
            // Run dir from cmd.exe as external command (this uses FindFirstFileW)
            (
                "dir via cmd",
                format!("^cmd.exe /c dir \"{}\"", tmp.path().display()),
            ),
            // Try creating a symlink and ls-ing that
            // Actually, let's try something simpler: does `ls -s` (short) work?
            ("ls -s file", format!("ls -s '{}'", file_path)),
            // What about ls with the directory and file combined?
            (
                "ls dir/file",
                format!("ls '{}/test.txt'", nu_path(tmp.path())),
            ),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 200 {
                        format!("{}...", &out.content[..200])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-FS {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-FS {}: ERR: {}", label, e),
            }
        }
    }

    /// Check path canonicalization inside AppContainer
    #[tokio::test]
    async fn diag_path_canonicalization() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("test.txt"), "content").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // What does nu see as PWD? Is it canonical?
        let tests = vec![
            ("pwd", "pwd".to_string()),
            ("env PWD", "$env.PWD".to_string()),
            // What does the Rust canonicalize() return inside the sandbox?
            ("path expand cwd", "'.' | path expand".to_string()),
            ("path expand file", "'test.txt' | path expand".to_string()),
            // What does glob return as paths? (wax-based)
            ("glob test.txt", "glob 'test.txt'".to_string()),
            // Check if there's a UNC/extended path difference
            (
                "path expand abs",
                format!("'{}' | path expand", nu_path(tmp.path())),
            ),
            // Does ls work with path from glob?
            (
                "ls glob-result",
                "let p = (glob 'test.txt' | first); ls $p".to_string(),
            ),
            // Does ls work on parent from path dirname?
            (
                "ls parent",
                "'test.txt' | path expand | path dirname | ls $in | length".to_string(),
            ),
            // What about `ls (path expand)`?
            ("ls expanded", "ls ('test.txt' | path expand)".to_string()),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 200 {
                        format!("{}...", &out.content[..200])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-PATH {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-PATH {}: ERR: {}", label, e),
            }
        }
    }

    /// Investigate why `open` returns nothing in AppContainer
    #[tokio::test]
    async fn diag_open_in_appcontainer() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("test.txt"), "hello world").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let tests = vec![
            // save works, so write is fine
            (
                "save + glob",
                "'written' | save 'w.txt' --force; glob 'w.txt'".to_string(),
            ),
            // Can we read back what we just wrote?
            ("open saved", "open 'w.txt' --raw | describe".to_string()),
            // Use `cat` alias?
            // Actually try: read file via std::io::BufReader equivalent
            // In nu, `open --raw` returns a byte stream from File::open
            // Under AppContainer, the File::open might succeed but the byte stream
            // might get closed/redirected
            // Let's check if `open` on a file that nu itself created works
            (
                "save+open",
                "'test data' | save 'x.txt' --force; open 'x.txt' --raw | describe".to_string(),
            ),
            // What about `open` on stdin piped from save?
            // Actually, let's test: does `open` fail because the byte stream is
            // immediately consumed/dropped by MCP serialization?
            // No — we already proved this works WITHOUT AppContainer.
            // So AppContainer specifically breaks `open` byte streams.
            // Let's try binary approach:
            ("open binary", "open 'test.txt' | describe".to_string()),
            // Try http get on file:// URL?
            // Try: scope commands to see if open is overridden
            ("which open", "which open | to json".to_string()),
            // Check: is stderr being swallowed? Maybe open prints errors to stderr
            // Let's try a command that explicitly reads the file differently
            // In nushell, can we use `from` commands?
            // Actually: does `stor` exist?
            // Let's just try to read bytes using alternative methods
            (
                "bytes length via save+open",
                "'hello' | save 'b.txt' --force; open 'b.txt' --raw | bytes length".to_string(),
            ),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 300 {
                        format!("{}...", &out.content[..300])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-OPEN {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-OPEN {}: ERR: {}", label, e),
            }
        }
    }

    /// Verify that lot's kill() actually terminates the inner child (nu).
    ///
    /// Hypothesis: on Linux, lot uses unshare(CLONE_NEWPID) which does NOT
    /// place the helper in the new PID namespace. The inner child is PID 1 in
    /// the new namespace. Killing the helper does NOT collapse the namespace,
    /// so the inner child (nu) survives and holds pipes open, causing hangs.
    ///
    /// This test checks process liveness via /proc rather than testing pipe
    /// behavior, to avoid leaking a blocked spawn_blocking task that would
    /// hang the tokio runtime shutdown.
    #[tokio::test]
    async fn diag_kill_closes_pipes() {
        skip_no_nu!();
        let project = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        try_spawn(&session, project.path(), ToolGrant::NU).await;

        // Get the child PID (helper PID) before killing
        let helper_pid = {
            let st = session.state.lock().await;
            let proc = st.process.as_ref().unwrap();
            let guard = proc.child_handle.lock().unwrap();
            guard.as_ref().map(|c| c.id())
        };
        eprintln!("DIAG-KILL: helper_pid = {:?}", helper_pid);

        // On Linux, find the inner child PID via /proc before killing
        #[cfg(target_os = "linux")]
        let inner_child_pids: Vec<u32> = if let Some(pid) = helper_pid {
            let children_path = format!("/proc/{pid}/task/{pid}/children");
            match std::fs::read_to_string(&children_path) {
                Ok(children) => {
                    let pids: Vec<u32> = children
                        .split_whitespace()
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    eprintln!("DIAG-KILL: helper children before kill: {:?}", pids);
                    pids
                }
                Err(e) => {
                    eprintln!("DIAG-KILL: could not read children: {e}");
                    vec![]
                }
            }
        } else {
            vec![]
        };

        // Kill via session.kill() (same path as timeout recovery)
        session.kill().await;
        eprintln!("DIAG-KILL: session.kill() completed");

        // Wait briefly for processes to die
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Check if the helper and inner child are still alive (Linux only)
        #[cfg(target_os = "linux")]
        {
            if let Some(pid) = helper_pid {
                let helper_status = format!("/proc/{pid}/status");
                match std::fs::read_to_string(&helper_status) {
                    Ok(status) => {
                        let state_line = status
                            .lines()
                            .find(|l| l.starts_with("State:"))
                            .unwrap_or("State: unknown");
                        eprintln!("DIAG-KILL: helper status after kill: {state_line}");
                    }
                    Err(_) => eprintln!("DIAG-KILL: helper /proc entry gone (reaped or dead)"),
                }
            }

            for inner_pid in &inner_child_pids {
                let status_path = format!("/proc/{inner_pid}/status");
                match std::fs::read_to_string(&status_path) {
                    Ok(status) => {
                        let state_line = status
                            .lines()
                            .find(|l| l.starts_with("State:"))
                            .unwrap_or("State: unknown");
                        let name_line = status
                            .lines()
                            .find(|l| l.starts_with("Name:"))
                            .unwrap_or("Name: unknown");
                        let ppid_line = status
                            .lines()
                            .find(|l| l.starts_with("PPid:"))
                            .unwrap_or("PPid: unknown");
                        eprintln!(
                            "DIAG-KILL: inner child {inner_pid} STILL ALIVE: \
                             {state_line}, {name_line}, {ppid_line}"
                        );
                        eprintln!(
                            "DIAG-KILL: CONFIRMED — lot's kill() does not terminate \
                             the inner child. The helper is NOT PID namespace init \
                             (it used unshare, not clone). Killing it orphans the \
                             inner child instead of collapsing the namespace."
                        );
                        // Clean up: kill the inner child directly so it doesn't
                        // leak and block subsequent tests
                        let _ = std::process::Command::new("kill")
                            .args(["-9", &inner_pid.to_string()])
                            .output();
                        eprintln!("DIAG-KILL: sent SIGKILL to inner child {inner_pid}");
                    }
                    Err(_) => {
                        eprintln!(
                            "DIAG-KILL: inner child {inner_pid} /proc entry gone — \
                             process terminated (namespace collapsed correctly)"
                        );
                    }
                }
            }

            if inner_child_pids.is_empty() {
                eprintln!(
                    "DIAG-KILL: could not find inner child PIDs — \
                     cannot verify kill behavior"
                );
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            eprintln!("DIAG-KILL: /proc inspection only available on Linux");
            let _ = helper_pid;
        }
    }

    /// Test with stderr captured to see if open prints errors there
    #[tokio::test]
    async fn diag_open_with_stderr() {
        skip_no_nu!();
        let cache = tmp_sandbox_cache();
        let cache_dir = cache.as_ref().map(|c| c.path());
        let nu_binary = resolve_nu_binary(cache_dir);
        let config_files = resolve_config_files(cache_dir);

        let project = tmp_sandbox_project();
        let test_file = project.path().join("test.txt");
        std::fs::write(&test_file, "stderr test").unwrap();

        let grant = ToolGrant::NU | ToolGrant::WRITE;
        let session_temp_base = project.path().join(".reel").join("tmp");
        std::fs::create_dir_all(&session_temp_base).unwrap_or(());
        let session_temp_dir = tempfile::TempDir::new_in(&session_temp_base).unwrap();

        let policy =
            build_nu_sandbox_policy(project.path(), grant, cache_dir, session_temp_dir.path())
                .unwrap();

        // Spawn with stderr PIPED instead of null
        let mut cmd = SandboxCommand::new(&nu_binary);
        cmd.arg("--mcp");
        if let Some((ref config_path, ref env_path)) = config_files {
            cmd.arg("--config").arg(config_path);
            cmd.arg("--env-config").arg(env_path);
        }
        cmd.cwd(project.path());
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped); // Capture stderr!
        cmd.stdin(SandboxStdio::Piped);
        cmd.env("TEMP", session_temp_dir.path());
        cmd.env("TMP", session_temp_dir.path());
        cmd.forward_common_env();

        let mut child = lot::spawn(&policy, &cmd).unwrap();
        let stdin = child.take_stdin().unwrap();
        let stdout = child.take_stdout().unwrap();
        let stderr = child.take_stderr().unwrap();

        let child_handle: ChildHandle = Arc::new(std::sync::Mutex::new(Some(child)));

        let stdin_handle: StdinHandle = Arc::new(std::sync::Mutex::new(Some(stdin)));
        let mut proc = NuProcess {
            stdin: stdin_handle,
            stdout: BufReader::new(stdout),
            next_id: 1,
            grant,
            project_root: project.path().to_path_buf(),
            child_handle: Arc::clone(&child_handle),
            _session_temp_dir: session_temp_dir,
        };

        // MCP initialize
        let init_request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 0,
            method: "initialize",
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "diag", "version": "0.1" }
            })),
        };
        let init_bytes = serde_json::to_vec(&init_request).unwrap();
        send_line(&proc.stdin, &init_bytes).unwrap();
        let _init_resp = read_response(&mut proc.stdout, 0).unwrap();

        // Send initialized notification
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let notif_bytes = serde_json::to_vec(&notif).unwrap();
        send_line(&proc.stdin, &notif_bytes).unwrap();

        // Run open command
        let file_path = nu_path(&test_file);
        let open_cmd = format!("open '{}' --raw | describe", file_path);
        let result = rpc_call(&mut proc, &open_cmd);
        eprintln!("DIAG-STDERR open result: {:?}", result);

        // Also try ls
        let ls_cmd = format!("ls '{}'", file_path);
        let result2 = rpc_call(&mut proc, &ls_cmd);
        eprintln!("DIAG-STDERR ls result: {:?}", result2);

        // Now read stderr with a timeout thread
        let mut stderr_buf = BufReader::new(stderr);
        use std::io::Read;
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut output = String::new();
            loop {
                match stderr_buf.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
                    Err(_) => break,
                }
                if output.len() > 8192 {
                    break;
                }
            }
            output
        });

        // Kill the process to unblock stderr read
        {
            let mut guard = child_handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut child) = *guard {
                let _ = child.kill();
            }
        }

        // Wait for stderr thread with a timeout — on Linux, lot's kill() sends
        // SIGKILL to the helper process but the inner child (nu, PID 1 in the
        // PID namespace) may survive because the helper used unshare(CLONE_NEWPID)
        // rather than clone(CLONE_NEWPID), so the helper is NOT PID namespace
        // init. If the inner child survives, it holds the stderr pipe open and
        // this join would block forever.
        let join_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stderr_result = loop {
            if stderr_handle.is_finished() {
                break Some(stderr_handle.join());
            }
            if std::time::Instant::now() >= join_deadline {
                eprintln!(
                    "DIAG-STDERR: stderr thread did not finish within 5s after kill — \
                     inner child likely survived helper kill (PID namespace not collapsed)"
                );
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };

        match stderr_result {
            Some(Ok(stderr_text)) => {
                if stderr_text.is_empty() {
                    eprintln!("DIAG-STDERR: stderr is empty");
                } else {
                    eprintln!(
                        "DIAG-STDERR: stderr content: {}",
                        &stderr_text[..stderr_text.len().min(2000)]
                    );
                }
            }
            Some(Err(_)) => eprintln!("DIAG-STDERR: stderr thread panicked"),
            None => {
                eprintln!("DIAG-STDERR: TIMEOUT — kill did not close stderr pipe");
            }
        }

        // Drop proc to close our end of the pipes — this will cause the
        // surviving inner child to get SIGPIPE on next write, eventually
        // killing it.
        drop(proc);
    }

    /// Narrow down the exact divergence between `path exists` (works) and
    /// `ls <file>` (fails) inside AppContainer.
    ///
    /// `path exists` → expand_path_with → Path::exists (fs::metadata)
    /// `ls <file>`   → glob_from → absolute_with → std::path::absolute → p.exists()
    ///                 also: nu_glob fill_todo → fs::metadata(path.join(s))
    ///
    /// The hypothesis: std::path::absolute produces a \\?\ prefixed path on
    /// Windows, and GetFileAttributesW on that form fails inside AppContainer
    /// even though the non-prefixed form succeeds.
    #[tokio::test]
    async fn diag_metadata_divergence() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("probe.txt"), "probe data").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let file_path = nu_path(&tmp.path().join("probe.txt"));
        let dir_path = nu_path(tmp.path());

        // Each test maps to a specific Rust/Win32 codepath:
        let tests = vec![
            // 1. path exists — expand_path_with then Path::exists (GetFileAttributesW)
            ("path-exists", format!("'{}' | path exists", file_path)),
            // 2. ls dir — read_dir (FindFirstFileW/FindNextFileW on dir\\*)
            (
                "ls-dir",
                format!(
                    "ls '{}' | where name ends-with 'probe.txt' | length",
                    dir_path
                ),
            ),
            // 3. ls file — goes through glob_from → absolute_with → exists check
            ("ls-file", format!("ls '{}'", file_path)),
            // 4. glob exact file — wax crate, different from nu_glob
            ("glob-exact", format!("glob '{}'", file_path)),
            // 5. path expand then ls — does path expand produce \\?\ prefix?
            ("path-expand", format!("'{}' | path expand", file_path)),
            // 6. ls on path-expand result — test if expanded path form breaks ls
            ("ls-expanded", format!("ls ('{}' | path expand)", file_path)),
            // 7. Relative path ls — nu_glob fill_todo with relative join
            ("ls-relative", "ls probe.txt".to_string()),
            // 8. symlink_metadata equivalent — path type uses this
            ("path-type", format!("'{}' | path type", file_path)),
            // 9. open — CreateFileW
            ("open", format!("open '{}' --raw | str length", file_path)),
            // 10. Test if the issue is \\?\ prefix specifically
            // Construct a \\?\ path manually and test exists
            (
                "exists-unc",
                format!(
                    r"'\\?\{}' | path exists",
                    tmp.path().join("probe.txt").display()
                ),
            ),
            // 11. Same with ls
            (
                "ls-unc",
                format!(r"ls '\\?\{}'", tmp.path().join("probe.txt").display()),
            ),
            // 12. std::path::absolute equivalent: path expand --no-symlink (closest nu equivalent)
            (
                "path-expand-nosym",
                format!("'{}' | path expand --no-symlink", file_path),
            ),
            // 13. Check what `which ls` says — confirm it's builtin
            ("which-ls", "which ls | get path.0".to_string()),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 300 {
                        format!("{}...", &out.content[..300])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-META {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-META {}: ERR: {}", label, e),
            }
        }
    }

    /// Probe whether glob_from's `absolute_with` path differs from
    /// `expand_path_with` inside AppContainer. The hypothesis is that
    /// `std::path::absolute` (GetFullPathNameW) produces a path that
    /// then fails `exists()` inside the sandbox.
    ///
    /// We test this by using nu to replicate what glob_from does:
    /// 1. expand_path_with → join cwd + relative, expand tilde/dots
    /// 2. std::path::absolute → GetFullPathNameW
    /// 3. Path::exists on the result
    #[tokio::test]
    async fn diag_glob_from_path_flow() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("probe.txt"), "probe data").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let file_path = nu_path(&tmp.path().join("probe.txt"));
        let dir_path = nu_path(tmp.path());

        // Replicate what glob_from does step by step inside nu.
        // glob_from calls: let path = expand_path_with(path, cwd, is_expand)
        //                  fs::symlink_metadata(&path)
        //                  absolute_with(path, cwd) → std::path::absolute(joined)
        //                  p.exists()
        //
        // In nu we can approximate these with:
        // - `path expand` for expand_path_with
        // - `path exists` for Path::exists
        // - For std::path::absolute we have no direct equivalent, but we can
        //   check if the EXPANDED path works with exists vs if ls sees it.

        let tests = vec![
            // Check symlink_metadata equivalent
            ("symlink-meta-dir", format!("'{}' | path type", dir_path)),
            ("symlink-meta-file", format!("'{}' | path type", file_path)),
            // Check if ls . works (read_dir path, not glob_from)
            ("ls-dot", "ls . | length".to_string()),
            // Check ls * (glob path in glob_from)
            ("ls-star", "ls * | length".to_string()),
            // Check if ls *.txt works (glob metachar present → different branch)
            ("ls-glob-star-txt", "ls *.txt | length".to_string()),
            // This is interesting: ls with a glob pattern that matches exactly
            // one file goes through the GLOB branch (nu_glob::glob_with),
            // not the non-glob branch (absolute_with). Does it work?
            ("ls-glob-q", "ls prob?.txt | length".to_string()),
            // ls with explicit glob wrapper
            ("ls-glob-exact", "ls (glob 'probe.txt' | first)".to_string()),
            // Test: does `path exists` on the absolute path work?
            (
                "exists-abs",
                format!("('{}' | path expand) | path exists", file_path),
            ),
            // Confirm file shows up in directory listing
            (
                "ls-dir-filter",
                format!(
                    "ls '{}' | where name ends-with probe.txt | get name",
                    dir_path
                ),
            ),
            // Try do/catch to get the actual error from ls
            (
                "ls-file-error",
                format!("try {{ ls '{}' }} catch {{ |e| $e | to json }}", file_path),
            ),
            // Check: does `ls` with `--all` flag change anything?
            ("ls-file-all", format!("ls -a '{}'", file_path)),
        ];

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    let content = if out.content.len() > 400 {
                        format!("{}...", &out.content[..400])
                    } else {
                        out.content.clone()
                    };
                    eprintln!("DIAG-GFROM {}: {}: {}", label, status, content);
                }
                Err(e) => eprintln!("DIAG-GFROM {}: ERR: {}", label, e),
            }
        }
    }

    /// PROOF: nu_glob traverses from root one component at a time, calling
    /// fs::metadata on each intermediate directory. AppContainer blocks
    /// metadata on intermediate dirs outside the grant.
    ///
    /// Verify by testing `path exists` on intermediate path components.
    #[tokio::test]
    async fn diag_intermediate_dir_metadata() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("probe.txt"), "probe data").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        // Build list of all intermediate paths from root to the test file.
        // E.g., for C:\UnitySrc\reel\reel\target\sandbox-test\.tmpXXX\probe.txt:
        //   C:\
        //   C:\UnitySrc
        //   C:\UnitySrc\reel
        //   ... etc ...
        //   C:\UnitySrc\reel\reel\target\sandbox-test\.tmpXXX
        //   C:\UnitySrc\reel\reel\target\sandbox-test\.tmpXXX\probe.txt
        let full_path = tmp.path().join("probe.txt");
        let mut ancestors: Vec<_> = full_path.ancestors().collect();
        ancestors.reverse(); // root first

        let mut tests = Vec::new();
        for ancestor in &ancestors {
            let p = nu_path(ancestor);
            if p.is_empty() {
                continue;
            }
            tests.push((
                format!("exists:{}", ancestor.display()),
                format!("'{}' | path exists", p),
            ));
        }

        // Also test path type on each (uses symlink_metadata)
        for ancestor in &ancestors {
            let p = nu_path(ancestor);
            if p.is_empty() {
                continue;
            }
            tests.push((
                format!("type:{}", ancestor.display()),
                format!("'{}' | path type", p),
            ));
        }

        for (label, cmd) in tests {
            let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
            match result {
                Ok(out) => {
                    let status = if out.is_error { "FAIL" } else { "OK" };
                    eprintln!("DIAG-INTER {}: {}: {}", label, status, out.content);
                }
                Err(e) => eprintln!("DIAG-INTER {}: ERR: {}", label, e),
            }
        }
    }

    // === END DIAGNOSTIC TESTS ===

    #[tokio::test]
    async fn integration_custom_command_reel_grep() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("searchable.txt"), "findme in this file\n").unwrap();
        let grant = ToolGrant::NU | ToolGrant::WRITE;
        let cmd = format!("reel grep 'findme' --path '{}'", nu_path(tmp.path()));
        let result = try_eval(session, &cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(!out.is_error, "reel grep failed: {}", out.content);
        assert!(out.content.contains("searchable.txt"));
    }

    #[tokio::test]
    async fn integration_timeout_kills_process() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        let result = try_eval(&session, "sleep 60sec", 2, tmp.path(), ToolGrant::NU).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("timed out"), "error: {err}");
        // Session recovers after timeout.
        let result2 = try_eval(&session, "echo 'recovered'", 30, tmp.path(), ToolGrant::NU).await;
        let out = result2.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("recovered"));
    }

    #[tokio::test]
    async fn integration_grant_change_respawns() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        let result = try_eval(&session, "echo 'ro'", 30, tmp.path(), ToolGrant::NU).await;
        let out1 = result.unwrap();
        assert!(!out1.is_error);
        // Switch to write grant — triggers respawn.
        let result2 = try_eval(
            &session,
            "echo 'rw'",
            30,
            tmp.path(),
            ToolGrant::NU | ToolGrant::WRITE,
        )
        .await;
        let out2 = result2.unwrap();
        assert!(!out2.is_error);
    }

    #[tokio::test]
    async fn integration_generation_prevents_stale_writeback() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        let (session, _cache) = isolated_session();
        try_spawn(&session, tmp.path(), ToolGrant::NU).await;
        let gen_before = {
            let st = session.state.lock().await;
            st.generation
        };
        session.kill().await;
        let gen_after = {
            let st = session.state.lock().await;
            st.generation
        };
        assert!(gen_after > gen_before);
        let st = session.state.lock().await;
        assert!(st.process.is_none());
    }

    #[tokio::test]
    async fn integration_env_filtering_rg_available() {
        skip_no_nu!();
        let tmp = tmp_sandbox_project();
        std::fs::write(tmp.path().join("needle.txt"), "haystack\n").unwrap();
        let (session, _cache) = isolated_session();
        let grant = ToolGrant::NU | ToolGrant::WRITE;
        // Use REEL_RG_PATH (absolute path) instead of bare `^rg`. NuShell's
        // PATH-based command lookup fails under AppContainer on Windows.
        let cmd = format!(
            "^$env.REEL_RG_PATH --color=never haystack '{}'",
            nu_path(tmp.path())
        );
        let result = try_eval(&session, &cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(
            !out.is_error,
            "rg not available in nu session: {}",
            out.content
        );
        assert!(out.content.contains("haystack"));
    }

    // -----------------------------------------------------------------------
    // Sandbox policy verification
    //
    // Each test uses sandbox_env() for isolated project and cache dirs,
    // eliminating shared state between concurrent tests.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn integration_sandbox_read_only_prevents_writes() {
        // read_path policy must block file creation/mutation inside the project root.
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        // Seed a file so we can also test overwrite prevention.
        std::fs::write(tmp.path().join("existing.txt"), "original").unwrap();
        // NU without WRITE — sandbox uses read_path for project root.
        let grant = ToolGrant::NU;

        // Attempt 1: create a new file inside the read-only project root.
        let write_cmd = format!(
            "'blocked' | save '{}'",
            nu_path(&tmp.path().join("new_file.txt"))
        );
        let result = try_eval(session, &write_cmd, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(
            out.is_error,
            "write should fail under read-only sandbox, got: {}",
            out.content
        );
        assert!(
            !tmp.path().join("new_file.txt").exists(),
            "file must not be created under read-only policy"
        );

        // Attempt 2: overwrite an existing file.
        let overwrite_cmd = format!(
            "'overwritten' | save --force '{}'",
            nu_path(&tmp.path().join("existing.txt"))
        );
        let out2 = session
            .evaluate(&overwrite_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        assert!(
            out2.is_error,
            "overwrite should fail under read-only sandbox, got: {}",
            out2.content
        );
        let content = std::fs::read_to_string(tmp.path().join("existing.txt")).unwrap();
        assert_eq!(content, "original", "file content must not change");

        // Attempt 3: mkdir inside the project root.
        let mkdir_cmd = format!("mkdir '{}'", nu_path(&tmp.path().join("subdir")));
        let out3 = session
            .evaluate(&mkdir_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        assert!(
            out3.is_error,
            "mkdir should fail under read-only sandbox, got: {}",
            out3.content
        );
        assert!(
            !tmp.path().join("subdir").exists(),
            "directory must not be created under read-only policy"
        );

        // Attempt 4: rm an existing file.
        let rm_cmd = format!("rm '{}'", nu_path(&tmp.path().join("existing.txt")));
        let out_rm = session
            .evaluate(&rm_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        assert!(
            out_rm.is_error,
            "rm should fail under read-only sandbox, got: {}",
            out_rm.content
        );
        assert!(
            tmp.path().join("existing.txt").exists(),
            "file must not be deleted under read-only policy"
        );

        // Attempt 5: mv (rename) an existing file.
        let mv_cmd = format!(
            "mv '{}' '{}'",
            nu_path(&tmp.path().join("existing.txt")),
            nu_path(&tmp.path().join("renamed.txt")),
        );
        let out_mv = session
            .evaluate(&mv_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        assert!(
            out_mv.is_error,
            "mv should fail under read-only sandbox, got: {}",
            out_mv.content
        );
        assert!(
            tmp.path().join("existing.txt").exists(),
            "original file must still exist after failed mv"
        );
        assert!(
            !tmp.path().join("renamed.txt").exists(),
            "renamed file must not exist under read-only policy"
        );

        // Attempt 6: rg (child process execution from exec_path).
        // Use REEL_RG_PATH (absolute path) — nu's PATH lookup fails under AppContainer.
        let rg_cmd = format!(
            "^$env.REEL_RG_PATH --color=never original '{}'",
            nu_path(tmp.path())
        );
        let out_rg = session
            .evaluate(&rg_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        #[cfg(target_os = "windows")]
        assert!(
            !out_rg.is_error,
            "rg failed in read-only sandbox. On Windows, AppContainer blocks child processes \
             unless the NUL device ACL is configured. Run the consumer's setup command from an elevated \
             (Administrator) prompt to fix. Raw error: {}",
            out_rg.content
        );
        #[cfg(not(target_os = "windows"))]
        assert!(
            !out_rg.is_error,
            "rg should be accessible in read-only sandbox: {}",
            out_rg.content
        );
    }

    #[tokio::test]
    async fn integration_sandbox_temp_dir_no_pivot_to_project() {
        // A read-only session can write to its per-session temp dir, but must
        // not be able to pivot that access back to the project root — e.g.
        // copy a file to temp, modify it, then write it back.
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        std::fs::write(tmp.path().join("source.txt"), "immutable content").unwrap();
        let grant = ToolGrant::NU;

        // Copy to a temp file, modify it, then attempt to write back.
        // This is the pivot attack: use temp dir write access to stage a
        // modified copy, then try to overwrite the project file.
        let pivot_cmd = format!(
            "let tmp = (mktemp); \
             open '{}' | save --force $tmp; \
             'tampered' | save --force $tmp; \
             open $tmp | save --force '{}'",
            nu_path(&tmp.path().join("source.txt")),
            nu_path(&tmp.path().join("source.txt")),
        );
        let out2 = session
            .evaluate(&pivot_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        // The final `save --force` back to project root must fail.
        assert!(
            out2.is_error,
            "pivot write-back should fail under read-only sandbox, got: {}",
            out2.content
        );
        let content = std::fs::read_to_string(tmp.path().join("source.txt")).unwrap();
        assert_eq!(
            content, "immutable content",
            "project file must remain unchanged after pivot attempt"
        );

        // Also try writing a new file to project root via temp staging.
        let pivot_new_cmd = format!(
            "let tmp = (mktemp); \
             'injected' | save --force $tmp; \
             cp $tmp '{}'",
            nu_path(&tmp.path().join("injected.txt")),
        );
        let out3 = session
            .evaluate(&pivot_new_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        assert!(
            out3.is_error,
            "cp from temp to project root should fail, got: {}",
            out3.content
        );
        assert!(
            !tmp.path().join("injected.txt").exists(),
            "injected file must not exist in project root"
        );
    }

    #[tokio::test]
    async fn integration_sandbox_write_grant_permits_writes() {
        // Write grant must allow file creation in the project root.
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let grant = ToolGrant::NU | ToolGrant::WRITE;

        let write_cmd = format!(
            "'hello' | save '{}'",
            nu_path(&tmp.path().join("created.txt"))
        );
        let out = session
            .evaluate(&write_cmd, 30, tmp.path(), grant)
            .await
            .unwrap();
        assert!(
            !out.is_error,
            "write should succeed with WRITE grant: {}",
            out.content
        );
        let content = std::fs::read_to_string(tmp.path().join("created.txt")).unwrap();
        assert_eq!(
            content, "hello",
            "file content should match what was written"
        );
    }

    // -----------------------------------------------------------------------
    // Sandbox rg diagnosis tests
    //
    // Root cause: AppContainer blocks access to \\.\NUL device. Nu's MCP
    // mode sets stdin(Stdio::null()) for external commands, which triggers
    // Rust's stdlib to open \\.\NUL via CreateFileW. AppContainer denies
    // this (ERROR_ACCESS_DENIED = 5). CreateProcessW itself works fine.
    // Fix: change nu's run_external.rs to use Stdio::piped() in MCP mode.
    // See docs/WINDOWS_SANDBOX.md for full investigation.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn integration_nosandbox_rg_executes() {
        // Control: rg works when invoked directly (no sandbox). Proves the
        // binary is present and functional, isolating AppContainer as the
        // cause when rg fails inside the sandbox.
        skip_no_nu!();
        let cache_dir = option_env!("NU_CACHE_DIR").map(std::path::Path::new);
        let Some(rg_binary) = resolve_rg_binary(cache_dir) else {
            eprintln!("skipping: rg binary not found");
            return;
        };

        // Invoke rg directly — no sandbox, no nu.
        let output = std::process::Command::new(&rg_binary)
            .arg("--version")
            .output()
            .expect("failed to execute rg binary");
        assert!(
            output.status.success(),
            "rg --version should succeed outside sandbox: exit={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("ripgrep"),
            "rg output should contain 'ripgrep', got: {}",
            stdout
        );
    }

    #[tokio::test]
    async fn integration_sandbox_rg_with_ancestor_traverse() {
        // Verifies rg child process execution inside AppContainer.
        // Requires NUL device ACL grant (via lot::grant_appcontainer_prerequisites) to pass.
        // Without it, nu's MCP mode opens \\.\NUL for child stdin,
        // AppContainer denies access (os error 5), and rg fails.
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let grant = ToolGrant::NU;

        let cache_path = match &env._cache {
            Some(c) => c.path().to_path_buf(),
            None => {
                eprintln!("SKIP: no NU_CACHE_DIR available");
                return;
            }
        };

        // Trigger session spawn (applies sandbox ACLs via lot).
        let init = try_eval(session, "echo 'init'", 30, tmp.path(), grant).await;
        let _ = init.unwrap();

        // Verify rg.exe has the AppContainer ACL (RX) via inheritance.
        let rg_exe = cache_path.join("rg.exe");
        if cfg!(windows) {
            let output = std::process::Command::new("icacls")
                .arg(rg_exe.as_os_str())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output();
            if let Ok(o) = output {
                let acl_text = String::from_utf8_lossy(&o.stdout);
                eprintln!("rg.exe ACLs:\n{acl_text}");
                assert!(
                    acl_text.contains("(I)(RX)"),
                    "rg.exe should have inherited RX ACL from exec_path"
                );
            }
        }

        // rg execution inside AppContainer: succeeds only if NUL device is accessible.
        let rg_full = format!("^'{}' --version", nu_path(&rg_exe));
        let result = try_eval(session, &rg_full, 30, tmp.path(), grant).await;
        let out = result.unwrap();
        assert!(
            !out.is_error,
            "rg execution failed inside AppContainer. This means the NUL device \
             ACL has not been configured. Run the consumer's setup command from an elevated \
             (Administrator) prompt, then re-run this test.\n\
             Error: {}",
            out.content
        );
        assert!(
            out.content.contains("ripgrep"),
            "expected rg --version output containing 'ripgrep', got: {}",
            out.content
        );
    }

    /// Diagnose whether the AppContainer blocks file READ access to rg.exe
    /// or specifically blocks CreateProcess (child process spawning).
    #[tokio::test]
    async fn integration_sandbox_diagnose_rg_access() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let grant = ToolGrant::NU;

        let cache_path = match &env._cache {
            Some(c) => c.path().to_path_buf(),
            None => {
                eprintln!("SKIP: no NU_CACHE_DIR available");
                return;
            }
        };

        // Trigger session spawn.
        let init = try_eval(session, "echo 'init'", 30, tmp.path(), grant).await;
        let _ = init.unwrap();

        let rg_exe = cache_path.join("rg.exe");

        // Test 1: Can nu stat rg.exe? Use ls on the cache directory.
        let read_cmd = format!("ls '{}' | length", nu_path(&cache_path));
        let read_result = try_eval(session, &read_cmd, 30, tmp.path(), grant).await;
        let read_out = read_result.unwrap();
        eprintln!(
            "File read rg.exe: is_error={}, content={}",
            read_out.is_error, read_out.content
        );

        // Test 2: Can nu READ a System32 DLL? (proves System32 access from inside AppContainer)
        let sys32_cmd = "open --raw 'C:/Windows/System32/kernel32.dll' | bytes length";
        let sys32_result = try_eval(session, sys32_cmd, 30, tmp.path(), grant).await;
        let sys32_out = sys32_result.unwrap();
        eprintln!(
            "File read kernel32.dll: is_error={}, content={}",
            sys32_out.is_error, sys32_out.content
        );

        // Test 3: Can nu execute cmd.exe from System32? (^cmd /C echo hi)
        let cmd_exec = "^'C:/Windows/System32/cmd.exe' /C echo hi";
        let cmd_result = try_eval(session, cmd_exec, 30, tmp.path(), grant).await;
        let cmd_out = cmd_result.unwrap();
        eprintln!(
            "Execute cmd.exe: is_error={}, content={}",
            cmd_out.is_error, cmd_out.content
        );

        // Test 4: Execute rg.exe with full path (expected to fail).
        let rg_exec = format!("^'{}' --version", nu_path(&rg_exe));
        let rg_result = try_eval(session, &rg_exec, 30, tmp.path(), grant).await;
        let rg_out = rg_result.unwrap();
        eprintln!(
            "Execute rg.exe: is_error={}, content={}",
            rg_out.is_error, rg_out.content
        );

        // Test 5: What does `which rg` say?
        let which_cmd = "which rg";
        let which_result = try_eval(session, which_cmd, 30, tmp.path(), grant).await;
        let which_out = which_result.unwrap();
        eprintln!(
            "which rg: is_error={}, content={}",
            which_out.is_error, which_out.content
        );

        // Test 6: Exact error for hostname.
        let hostname_cmd = "^'C:/Windows/System32/hostname.exe'";
        let hostname_result = try_eval(session, hostname_cmd, 30, tmp.path(), grant).await;
        let hostname_out = hostname_result.unwrap();
        eprintln!(
            "Execute hostname.exe: is_error={}, content={}",
            hostname_out.is_error, hostname_out.content
        );

        // Test 7: Can nu list System32 directory? (proves directory traverse works)
        let ls_sys32 = "ls C:/Windows/System32/cmd.exe | get name.0";
        let ls_result = try_eval(session, ls_sys32, 30, tmp.path(), grant).await;
        let ls_out = ls_result.unwrap();
        eprintln!(
            "ls cmd.exe: is_error={}, content={}",
            ls_out.is_error, ls_out.content
        );

        // Test 8: Use sys/exec (Rust std::process::Command) to check OS error code
        let exec_cmd = format!("do {{ ^'{}' --version }} | complete", nu_path(&rg_exe));
        let exec_result = try_eval(session, &exec_cmd, 30, tmp.path(), grant).await;
        let exec_out = exec_result.unwrap();
        eprintln!(
            "complete rg exec: is_error={}, content={}",
            exec_out.is_error, exec_out.content
        );
    }

    /// Test whether nu inside AppContainer can READ rg.exe as raw bytes.
    /// If readable but not executable, the issue is specifically CreateProcess
    /// being blocked, not file access. If unreadable, the issue is ACLs.
    #[tokio::test]
    async fn integration_sandbox_rg_file_readable() {
        skip_no_nu!();
        let env = sandbox_env();
        let tmp = &env.project;
        let session = &env.session;
        let grant = ToolGrant::NU;

        let cache_path = match &env._cache {
            Some(c) => c.path().to_path_buf(),
            None => {
                eprintln!("SKIP: no NU_CACHE_DIR available");
                return;
            }
        };

        // Trigger session spawn (applies sandbox ACLs via lot).
        let init = try_eval(session, "echo 'init'", 30, tmp.path(), grant).await;
        let _ = init.unwrap();

        let rg_exe = cache_path.join("rg.exe");

        // Test 1: Can nu LIST the cache directory (proves directory traversal)?
        let ls_cmd = format!("ls '{}' | length", nu_path(&cache_path));
        let ls_result = try_eval(session, &ls_cmd, 30, tmp.path(), grant).await;
        let ls_out = ls_result.unwrap();
        eprintln!(
            "ls cache dir: is_error={}, content={}",
            ls_out.is_error, ls_out.content
        );

        // Test 2: Can nu READ rg.exe as raw bytes (proves file read access)?
        let read_cmd = format!("open --raw '{}' | length", nu_path(&rg_exe));
        let read_result = try_eval(session, &read_cmd, 30, tmp.path(), grant).await;
        let read_out = read_result.unwrap();
        eprintln!(
            "read rg.exe bytes: is_error={}, content={}",
            read_out.is_error, read_out.content
        );

        // Test 3: Can nu EXECUTE rg.exe (expected to fail)?
        let exec_cmd = format!("^'{}' --version", nu_path(&rg_exe));
        let exec_result = try_eval(session, &exec_cmd, 30, tmp.path(), grant).await;
        let exec_out = exec_result.unwrap();
        eprintln!(
            "exec rg.exe: is_error={}, content={}",
            exec_out.is_error, exec_out.content
        );

        // Conclusion: if read succeeds but exec fails, CreateProcess is
        // specifically blocked inside the AppContainer.
        if !read_out.is_error && exec_out.is_error {
            eprintln!(
                "CONCLUSION: File READ works but CreateProcess fails. \
                 AppContainer blocks child process spawning from inside the container."
            );
        } else if read_out.is_error {
            eprintln!("CONCLUSION: File READ is also blocked — ACL issue.");
        } else {
            eprintln!("CONCLUSION: Both read and exec succeeded — sandbox is not restricting.");
        }
    }
}
