//! Agent executor abstraction (Deliverable 9).
//!
//! `AgentExecutor` is the seam between mech's prompt block executor and the
//! agent runtime (reel). It lives in mech so tests can inject a deterministic
//! fake agent without mocking reel internals.
//!
//! Request and response types are plain serde-friendly structs: mech builds
//! an [`AgentRequest`] from the resolved agent cascade plus the rendered
//! prompt and the declared output schema, and the executor returns a raw
//! [`serde_json::Value`] output which mech then validates against the
//! declared schema.
//!
//! The production impl that wraps `reel::Agent` will land alongside the
//! function/workflow driver in a later deliverable — the full reel
//! [`reel::RequestConfig`] wiring (provider registry, structured-output
//! plumbing, tool registry) is an executor-wiring concern that does not
//! belong inside a single prompt-block dispatch. The trait shape is what D9
//! locks down.

use std::pin::Pin;
use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::conversation::Message;
use crate::error::MechError;

/// Owned boxed future alias used by [`AgentExecutor::run`]. A local alias
/// keeps us from depending on the `futures` crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// A single agent invocation request, built from the resolved agent-config
/// cascade for one prompt block execution.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequest {
    /// Resolved model name (e.g. `"opus"`). `None` means "executor default".
    pub model: Option<String>,
    /// Rendered system prompt (workflow-level `system` with function-level
    /// override applied), or `None` if none was configured.
    pub system: Option<String>,
    /// Rendered user prompt (the block's `prompt:` template after
    /// interpolation against the current execution context).
    pub prompt: String,
    /// ToolGrant flag strings (`tools`, `write`, `network`).
    pub grants: Vec<String>,
    /// Custom tool names to enable.
    pub tools: Vec<String>,
    /// Writable paths (relative to project root).
    pub write_paths: Vec<String>,
    /// Per-block timeout, if configured.
    pub timeout: Option<Duration>,
    /// JSON Schema document the output must conform to. Mech validates the
    /// returned value against this schema after the executor returns.
    pub output_schema: JsonValue,
    /// Conversation history from prior prompt blocks in the same function.
    /// Empty for the first prompt block or for dataflow blocks.
    pub history: Vec<Message>,
}

/// Response returned by an [`AgentExecutor`].
#[derive(Debug, Clone, PartialEq)]
pub struct AgentResponse {
    /// Raw JSON output. Validated by mech against the request's output
    /// schema; a mismatch is surfaced as
    /// [`MechError::SchemaValidationFailure`].
    pub output: JsonValue,
    /// Messages generated during this agent turn: at minimum the user
    /// prompt and assistant response. May include tool call/result pairs
    /// from the agent's internal loop (reel tool loop).
    pub messages: Vec<Message>,
}

/// The agent-runtime seam. Implementors dispatch an [`AgentRequest`] and
/// return an [`AgentResponse`].
///
/// A [`BoxFuture`]-returning signature (rather than `async fn in trait`)
/// keeps the trait object-safe so `&dyn AgentExecutor` works in function
/// signatures without nightly features.
pub trait AgentExecutor: Send + Sync {
    /// Dispatch an agent request.
    fn run<'a>(&'a self, request: AgentRequest) -> BoxFuture<'a, Result<AgentResponse, MechError>>;
}
