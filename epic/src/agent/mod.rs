// Agent abstraction over Flick agent runtime (external executable).

mod config_gen;
pub mod flick;
mod models;
mod prompts;
pub mod tools;

use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{LeafResult, Model, Task, TaskId, TaskOutcome};

/// Summary of a completed sibling task, provided as context to agent calls.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `id` field used in prompt formatting and tests.
pub struct SiblingSummary {
    pub id: TaskId,
    pub goal: String,
    pub outcome: TaskOutcome,
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
    ) -> impl std::future::Future<Output = anyhow::Result<AssessmentResult>> + Send;

    /// Execute a leaf task directly with the given model.
    fn execute_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> impl std::future::Future<Output = anyhow::Result<LeafResult>> + Send;

    /// Design decomposition and produce subtask specs.
    fn design_and_decompose(
        &self,
        ctx: &TaskContext,
    ) -> impl std::future::Future<Output = anyhow::Result<DecompositionResult>> + Send;

    /// Independent verification of a completed task.
    fn verify(
        &self,
        ctx: &TaskContext,
    ) -> impl std::future::Future<Output = anyhow::Result<VerificationResult>> + Send;

    /// Inter-subtask checkpoint after a child reports discoveries.
    fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> impl std::future::Future<Output = anyhow::Result<CheckpointDecision>> + Send;

    /// Assess whether recovery is possible after a child failure. Placeholder for v1.
    fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<String>>> + Send;
}
