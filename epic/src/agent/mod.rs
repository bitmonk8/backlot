// Agent abstraction over reel agent runtime (library dependency).

mod prompts;
pub mod reel_adapter;
pub mod wire;

use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{LeafResult, Model, RecoveryPlan, Task, TaskId, TaskOutcome};

// Re-export context types from cue.
pub use cue::{AgentResult, ChildStatus, ChildSummary, SessionMeta, SiblingSummary};

/// Context bundle passed to every agent call.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub task: Task,
    pub parent_goal: Option<String>,
    pub ancestor_goals: Vec<String>,
    pub completed_siblings: Vec<SiblingSummary>,
    pub pending_sibling_goals: Vec<String>,
    pub checkpoint_guidance: Option<String>,
    pub children: Vec<ChildSummary>,
    pub parent_discoveries: Vec<String>,
    pub parent_decomposition_rationale: Option<String>,
}

/// Extract metadata from a reel `RunResult`.
pub fn session_meta_from_run_result<T>(r: &reel::RunResult<T>) -> SessionMeta {
    let (input_tokens, output_tokens, cache_creation, cache_read, cost) =
        r.usage.as_ref().map_or((0, 0, 0, 0, 0.0), |u| {
            (
                u.input_tokens,
                u.output_tokens,
                u.cache_creation_input_tokens,
                u.cache_read_input_tokens,
                u.cost_usd,
            )
        });
    let total_latency_ms: u64 = r.transcript.iter().filter_map(|t| t.api_latency_ms).sum();
    SessionMeta {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        cost_usd: cost,
        tool_calls: r.tool_calls,
        total_latency_ms,
    }
}

/// Extract metadata from vault `SessionMetadata`.
pub fn session_meta_from_vault(meta: &vault::SessionMetadata) -> SessionMeta {
    SessionMeta {
        input_tokens: meta.input_tokens,
        output_tokens: meta.output_tokens,
        cache_creation_input_tokens: meta.cache_creation_input_tokens,
        cache_read_input_tokens: meta.cache_read_input_tokens,
        cost_usd: meta.cost_usd,
        tool_calls: meta.tool_calls,
        total_latency_ms: meta.api_latency_ms(),
    }
}

/// Trait abstracting all agent interactions.
pub trait AgentService: Send + Sync {
    fn assess(
        &self,
        ctx: &TaskContext,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<AssessmentResult>>> + Send;

    fn execute_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<LeafResult>>> + Send;

    fn design_and_decompose(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<DecompositionResult>>> + Send;

    fn verify(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<VerificationResult>>> + Send;

    fn file_level_review(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<VerificationResult>>> + Send;

    fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<CheckpointDecision>>> + Send;

    fn fix_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
        failure_reason: &str,
        attempt: u32,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<LeafResult>>> + Send;

    fn design_fix_subtasks(
        &self,
        ctx: &TaskContext,
        model: Model,
        verification_issues: &str,
        round: u32,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<DecompositionResult>>> + Send;

    fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<Option<String>>>> + Send;

    fn design_recovery_subtasks(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
        strategy: &str,
        recovery_round: u32,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<RecoveryPlan>>> + Send;
}
