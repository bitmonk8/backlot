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

    let t0 = std::time::Instant::now();
    let policy = build_nu_sandbox_policy(project_root, grant, cache_dir, session_temp_dir.path())
        .map_err(|e| format!("sandbox setup failed: {e}"))?;
    let t_policy = t0.elapsed();

    let nu_binary = resolve_nu_binary(cache_dir);
    let config_files = resolve_config_files(cache_dir);
    let rg_binary = resolve_rg_binary(cache_dir);
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let t_blocking = std::time::Instant::now();
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

        let t_pre_spawn = t_blocking.elapsed();
        let mut child =
            lot::spawn(&policy, &cmd).map_err(|e| format!("failed to spawn nu: {e}"))?;
        let t_lot_spawn = t_blocking.elapsed();

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

        let t_pre_handshake = t_blocking.elapsed();
        send_line(&proc.stdin, &init_bytes)?;

        // Read initialize response (uses skip loop like rpc_call).
        let init_response = read_response(&mut proc.stdout, 0)?;
        let t_handshake = t_blocking.elapsed();

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

        let t_total = t_blocking.elapsed();
        eprintln!(
            "[SPAWN-TIMING] policy={t_policy:?} cmd_setup={t_pre_spawn:?} lot_spawn={:?} pre_handshake={t_pre_handshake:?} handshake={:?} total={t_total:?}",
            t_lot_spawn.saturating_sub(t_pre_spawn),
            t_handshake.saturating_sub(t_pre_handshake),
        );

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
        // Primary assertion: the file must not exist in the project root.
        // On Linux, nu's `cp` may not report an error even when the write
        // is blocked by a read-only bind mount, so we check the filesystem
        // state rather than relying on `is_error`.
        assert!(
            !tmp.path().join("injected.txt").exists(),
            "injected file must not exist in project root (sandbox leak). \
             cp output: {}",
            out3.content
        );
        if !out3.is_error {
            eprintln!(
                "NOTE: cp to read-only project root did not report an error \
                 (platform-specific nu behavior). Sandbox still enforced — \
                 file does not exist."
            );
        }
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

        // Verify rg binary has the AppContainer ACL (RX) via inheritance.
        let rg_name = if cfg!(windows) { "rg.exe" } else { "rg" };
        let rg_exe = cache_path.join(rg_name);
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

        let rg_name = if cfg!(windows) { "rg.exe" } else { "rg" };
        let rg_exe = cache_path.join(rg_name);

        // Test 1: Can nu stat the rg binary? Use ls on the cache directory.
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

        let rg_name = if cfg!(windows) { "rg.exe" } else { "rg" };
        let rg_exe = cache_path.join(rg_name);

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
