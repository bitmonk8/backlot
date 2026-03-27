// Agent abstraction over reel agent runtime (library dependency).

mod prompts;
pub mod reel_adapter;
pub mod wire;

use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{LeafResult, Model, RecoveryPlan, Task, TaskId, TaskOutcome};

/// Summary of a completed sibling task, provided as context to agent calls.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `id` field used in prompt formatting and tests.
pub struct SiblingSummary {
    pub id: TaskId,
    pub goal: String,
    pub outcome: TaskOutcome,
    pub discoveries: Vec<String>,
}

/// Status of a child subtask (pending, in-progress, completed, or failed).
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants used in orchestrator and prompt formatting.
pub enum ChildStatus {
    Completed,
    Failed { reason: String },
    Pending,
    InProgress,
}

/// Summary of a child subtask, used in branch-task context.
#[derive(Debug, Clone)]
pub struct ChildSummary {
    pub goal: String,
    pub status: ChildStatus,
    pub discoveries: Vec<String>,
}

/// Context bundle passed to every agent call.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub task: Task,
    pub parent_goal: Option<String>,
    pub ancestor_goals: Vec<String>,
    pub completed_siblings: Vec<SiblingSummary>,
    pub pending_sibling_goals: Vec<String>,
    /// Adjustment guidance from a checkpoint decision, if any.
    pub checkpoint_guidance: Option<String>,
    /// Child subtask summaries (populated for branch tasks).
    pub children: Vec<ChildSummary>,
    /// Discoveries from the parent task, useful for recovery decisions.
    pub parent_discoveries: Vec<String>,
    /// Rationale the parent used when decomposing into subtasks.
    pub parent_decomposition_rationale: Option<String>,
}

/// Metadata from a single agent session (one `run_request` call).
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
    pub tool_calls: u32,
    pub total_latency_ms: u64,
}

impl SessionMeta {
    /// Extract metadata from a reel `RunResult`.
    pub fn from_run_result<T>(r: &reel::RunResult<T>) -> Self {
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
        Self {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
            cost_usd: cost,
            tool_calls: r.tool_calls,
            total_latency_ms,
        }
    }
}

/// Agent call result with observability metadata.
#[derive(Debug)]
pub struct AgentResult<T> {
    pub value: T,
    pub meta: SessionMeta,
}

/// Trait abstracting all agent interactions.
///
/// Generic (not `dyn`) — one concrete implementation per run.
/// `async fn` in trait works natively in edition 2024.
pub trait AgentService: Send + Sync {
    /// Determine leaf vs branch path and select a model tier.
    fn assess(
        &self,
        ctx: &TaskContext,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<AssessmentResult>>> + Send;

    /// Execute a leaf task directly with the given model.
    fn execute_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<LeafResult>>> + Send;

    /// Design decomposition and produce subtask specs.
    fn design_and_decompose(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<DecompositionResult>>> + Send;

    /// Independent verification of a completed task.
    fn verify(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<VerificationResult>>> + Send;

    /// Inter-subtask checkpoint after a child reports discoveries.
    fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<CheckpointDecision>>> + Send;

    /// Re-execute a leaf task with verification failure context.
    fn fix_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
        failure_reason: &str,
        attempt: u32,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<LeafResult>>> + Send;

    /// Design fix subtasks to address branch verification issues.
    fn design_fix_subtasks(
        &self,
        ctx: &TaskContext,
        model: Model,
        verification_issues: &str,
        round: u32,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<DecompositionResult>>> + Send;

    /// Assess whether recovery is possible after a child failure.
    fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<Option<String>>>> + Send;

    /// Design recovery subtasks after a child failure (Opus).
    /// `strategy` comes from `assess_recovery`. Returns a recovery plan with
    /// fresh subtasks and an incremental-vs-full decision.
    fn design_recovery_subtasks(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
        strategy: &str,
        recovery_round: u32,
    ) -> impl std::future::Future<Output = anyhow::Result<AgentResult<RecoveryPlan>>> + Send;
}
