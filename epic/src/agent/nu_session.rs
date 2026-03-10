// MCP client for a persistent `nu --mcp` process.
//
// Manages the lifecycle of one NuShell MCP server process per agent session.
// The process is spawned eagerly at session creation (for tool-granted sessions)
// and killed when the session ends or on timeout.
//
// Protocol: JSON-RPC 2.0 over stdio. Each message is a single JSON line
// terminated by `\n`.

use crate::agent::tools::ToolGrant;
use lot::{SandboxCommand, SandboxPolicyBuilder, SandboxStdio};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Output from a `NuShell` MCP `evaluate` call.
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

#[derive(Deserialize)]
struct JsonRpcResponse {
    id: Option<u64>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
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

struct NuProcess {
    stdin: File,
    stdout: BufReader<File>,
    next_id: u64,
    /// The grant under which this process was spawned (determines sandbox policy).
    grant: ToolGrant,
    /// Project root the sandbox is anchored to.
    project_root: PathBuf,
    /// Shared handle to the child — kept alive for cleanup, accessible for kill.
    child_handle: ChildHandle,
}

impl Drop for NuProcess {
    fn drop(&mut self) {
        let mut guard = self.child_handle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ref mut child) = *guard {
            let _ = child.kill();
        }
    }
}

/// Combined session state behind a single mutex to prevent lock-ordering deadlocks.
struct SessionState {
    process: Option<NuProcess>,
    generation: u64,
    /// Shared child handle kept here so `kill()` can reach the child even when
    /// the `NuProcess` has been taken out for blocking I/O in `evaluate_inner`.
    inflight_child: Option<ChildHandle>,
}

/// Manages a persistent `nu --mcp` process.
///
/// Thread-safe via internal `Mutex`. The process is spawned eagerly via
/// `spawn()` and restarted if the grant or project root changes between calls.
pub struct NuSession {
    state: Mutex<SessionState>,
}

/// Write a JSON-RPC message as a single `\n`-terminated line.
fn send_line(sink: &mut File, payload: &[u8]) -> Result<(), String> {
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
            state: Mutex::new(SessionState {
                process: None,
                generation: 0,
                inflight_child: None,
            }),
        }
    }

    /// Eagerly spawn the nu MCP process so it is warm by the first tool call.
    pub async fn spawn(&self, project_root: &Path, grant: ToolGrant) -> Result<(), String> {
        let mut st = self.state.lock().await;
        if st.process.is_some() {
            return Ok(());
        }
        st.generation += 1;
        let proc = spawn_nu_process(project_root, grant).await?;
        st.process = Some(proc);
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
            let mut guard = handle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut child) = *guard {
                let _ = child.kill();
            }
        }
        st.inflight_child = None;

        // Kill the process if it's parked in state (not currently in-flight).
        if let Some(proc) = st.process.take() {
            let mut child_guard = proc.child_handle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
                let new_proc = spawn_nu_process(project_root, grant).await?;
                st.process = Some(new_proc);
            }

            let proc = st.process.take().expect("process just spawned");
            st.inflight_child = Some(Arc::clone(&proc.child_handle));
            let generation = st.generation;
            drop(st);
            (proc, generation)
        };
        // Lock released — blocking I/O below does not hold the async mutex,
        // allowing timeout + kill() to work. kill() can reach the child via
        // inflight_child, which causes read_line to return EOF and unblocks
        // the spawn_blocking thread.

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
        if result.is_ok() && st.generation == generation_at_start {
            st.process = Some(proc);
        } else if result.is_err() {
            // Kill the process on RPC error to avoid leaking it.
            let mut child_guard = child_handle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ref mut child) = *child_guard {
                let _ = child.kill();
            }
            // proc is dropped here, NuProcess::Drop will also attempt kill (idempotent).
        }

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
                "command": command
            }
        })),
    };

    let request_bytes = serde_json::to_vec(&request)
        .map_err(|e| format!("failed to serialize request: {e}"))?;

    send_line(&mut proc.stdin, &request_bytes)?;

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
/// 2. Build-time cache directory (set by build.rs via `NU_CACHE_DIR`).
/// 3. Bare name on PATH.
fn resolve_cached_binary(binary_name: &str) -> OsString {
    // 1. Next to the current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(binary_name);
            if candidate.exists() {
                return candidate.into_os_string();
            }
        }
    }

    // 2. Build-time cache directory (nu and rg share the same directory).
    if let Some(cache_dir) = option_env!("NU_CACHE_DIR") {
        let candidate = Path::new(cache_dir).join(binary_name);
        if candidate.exists() {
            return candidate.into_os_string();
        }
    }

    // 3. PATH fallback.
    OsString::from(binary_name)
}

fn resolve_nu_binary() -> OsString {
    resolve_cached_binary(if cfg!(windows) { "nu.exe" } else { "nu" })
}

/// Resolve the path to the `rg` (ripgrep) binary. Used by `epic_grep` inside
/// the nu session — the resolved path is passed via environment variable so
/// `^rg` works inside the sandbox.
#[allow(dead_code)] // Used in a later phase (unified tool layer)
pub fn resolve_rg_binary() -> OsString {
    resolve_cached_binary(if cfg!(windows) { "rg.exe" } else { "rg" })
}

/// Build the sandbox policy for the nu process.
fn build_nu_sandbox_policy(
    project_root: &Path,
    grant: ToolGrant,
) -> lot::Result<lot::SandboxPolicy> {
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

/// Spawn a `nu --mcp` process inside a lot sandbox and perform the MCP
/// initialization handshake. The entire spawn + handshake runs on a blocking
/// thread to avoid blocking the async runtime.
async fn spawn_nu_process(project_root: &Path, grant: ToolGrant) -> Result<NuProcess, String> {
    let policy = build_nu_sandbox_policy(project_root, grant)
        .map_err(|e| format!("sandbox setup failed: {e}"))?;

    let nu_binary = resolve_nu_binary();
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = SandboxCommand::new(&nu_binary);
        cmd.arg("--mcp");
        cmd.cwd(&project_root);
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Null);
        cmd.stdin(SandboxStdio::Piped);
        cmd.forward_common_env();

        let mut child =
            lot::spawn(&policy, &cmd).map_err(|e| format!("failed to spawn nu: {e}"))?;

        let stdin = child.take_stdin().ok_or("failed to capture nu stdin")?;
        let stdout = child.take_stdout().ok_or("failed to capture nu stdout")?;

        let child_handle: ChildHandle = Arc::new(std::sync::Mutex::new(Some(child)));

        let mut proc = NuProcess {
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            grant,
            project_root,
            child_handle,
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
                    "name": "epic",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        };

        let init_bytes = serde_json::to_vec(&init_request)
            .map_err(|e| format!("failed to serialize init request: {e}"))?;

        send_line(&mut proc.stdin, &init_bytes)?;

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

        send_line(&mut proc.stdin, &notif_bytes)?;

        Ok(proc)
    })
    .await
    .map_err(|e| format!("spawn task panicked: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_nu_sandbox_policy_write_grant() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policy = build_nu_sandbox_policy(tmp.path(), ToolGrant::WRITE | ToolGrant::NU).unwrap();
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
    }

    #[test]
    fn test_build_nu_sandbox_policy_no_write_grant() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policy = build_nu_sandbox_policy(tmp.path(), ToolGrant::NU).unwrap();
        let canon = tmp.path().canonicalize().unwrap();

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
    fn test_build_nu_sandbox_policy_allows_network() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policy = build_nu_sandbox_policy(tmp.path(), ToolGrant::NU).unwrap();
        assert!(policy.allow_network);
    }

    #[test]
    fn test_build_nu_sandbox_policy_has_exec_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policy = build_nu_sandbox_policy(tmp.path(), ToolGrant::NU).unwrap();
        assert!(
            !policy.exec_paths.is_empty(),
            "exec_paths should contain platform directories"
        );
    }
}
