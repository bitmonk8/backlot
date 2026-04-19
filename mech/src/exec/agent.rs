//! Agent executor abstraction.
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
//! The production implementation that wraps `reel::Agent` lives alongside the
//! function/workflow driver. Full reel `reel::RequestConfig` wiring
//! (provider registry, structured-output plumbing, tool registry) is an
//! executor-wiring concern that does not belong inside a single prompt-block
//! dispatch — this trait keeps that surface narrow.

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
///
/// Contract: the rendered system prompt is conveyed via [`Self::system`]
/// only — never as the first element of [`Self::history`]. The first
/// element of [`Self::history`] (if present) is always a
/// [`Role::User`](crate::conversation::Role::User) message — the user
/// prompt of the immediately preceding turn — and the
/// list is empty for the first prompt block in a function and for
/// dataflow blocks. Implementors of [`AgentExecutor`] MUST consume
/// [`Self::system`] and MUST NOT look for a system-role message at
/// `history[0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequest {
    /// Resolved model name (e.g. `"opus"`). `None` means "executor default".
    pub model: Option<String>,
    /// Rendered system prompt (workflow-level `system` with function-level
    /// override applied), or `None` if none was configured. This is the
    /// sole carrier of the system prompt; see [`Self::history`] — the
    /// system prompt is never injected as a message there.
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
    /// Empty for the first prompt block or for dataflow blocks. The first
    /// element (if present) is always a
    /// [`Role::User`](crate::conversation::Role::User) message (the user
    /// prompt of the previous turn); the rendered system prompt lives in
    /// [`Self::system`] and is never duplicated here. The seam in
    /// `execute_prompt_block` enforces this with a `debug_assert!`.
    pub history: Vec<Message>,
}

/// Response returned by an [`AgentExecutor`].
#[derive(Debug, Clone, PartialEq)]
pub struct AgentResponse {
    /// Raw JSON output. Validated by mech against the request's output
    /// schema; a mismatch is surfaced as
    /// [`MechError::SchemaValidationFailure`].
    pub output: JsonValue,
    /// Messages generated during this agent turn — typically the user
    /// prompt, assistant response, and any intermediate tool call/result
    /// pairs from the agent's internal loop (reel tool loop). May be
    /// empty, in which case `execute_prompt_block` synthesizes a
    /// user+assistant pair from the request prompt and the validated
    /// output and appends that pair to the conversation instead.
    ///
    /// Contract: when non-empty, the list MUST begin with a
    /// [`Role::User`](crate::conversation::Role::User) message (the user
    /// turn that initiated this agent invocation). The seam in
    /// `execute_prompt_block` enforces this with a `debug_assert!` after
    /// dispatch — mech appends these messages verbatim to the
    /// conversation, so a non-User-leading list would propagate into the
    /// next [`AgentRequest::history`] and violate that field's invariant.
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
