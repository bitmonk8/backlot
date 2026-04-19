// TaskNode and TaskStore trait definitions.

use crate::config::LimitsConfig;
use crate::context::TreeContext;
use crate::orchestrator::OrchestratorError;
use crate::types::{
    AssessmentResult, BranchVerifyOutcome, ChildResponse, DecompositionResult, FixBudgetCheck,
    Model, RecoveryDecision, RecoveryEligibility, RegistrationInfo, ResumePoint, ScopeCheck,
    SessionMeta, SubtaskSpec, TaskId, TaskOutcome, TaskPath, TaskPhase,
};
use std::path::Path;

/// Data access, decisions, mutations, and lifecycle for a single task.
/// Implemented by the application crate's concrete task type.
pub trait TaskNode: Send {
    // --- Read accessors ---

    fn id(&self) -> TaskId;
    fn parent_id(&self) -> Option<TaskId>;
    fn goal(&self) -> &str;
    fn depth(&self) -> u32;
    fn phase(&self) -> TaskPhase;
    fn subtask_ids(&self) -> &[TaskId];
    fn discoveries(&self) -> &[String];
    fn recovery_rounds(&self) -> u32;

    // --- Decision methods ---

    fn is_terminal(&self) -> bool;
    fn resume_point(&self) -> ResumePoint;
    fn forced_assessment(&self, max_depth: u32) -> Option<AssessmentResult>;
    fn needs_decomposition(&self) -> bool;
    fn decompose_model(&self) -> Model;
    fn registration_info(&self) -> RegistrationInfo;

    // --- Mutations ---

    fn set_phase(&mut self, phase: TaskPhase);
    fn set_assessment(
        &mut self,
        path: TaskPath,
        model: Model,
        magnitude: Option<crate::types::Magnitude>,
    );
    fn set_decomposition_rationale(&mut self, rationale: String);
    fn set_subtask_ids(&mut self, ids: &[TaskId], append: bool);
    fn increment_fix_rounds(&mut self) -> u32;
    fn accumulate_usage(&mut self, meta: &SessionMeta) -> f64;

    // --- Lifecycle (async) ---

    fn execute_leaf(
        &mut self,
        ctx: &TreeContext,
    ) -> impl std::future::Future<Output = TaskOutcome> + Send;

    fn verify_branch(
        &mut self,
        ctx: &TreeContext,
    ) -> impl std::future::Future<Output = Result<BranchVerifyOutcome, OrchestratorError>> + Send;

    fn fix_round_budget_check(&self, limits: &LimitsConfig) -> FixBudgetCheck;

    fn check_branch_scope(&self) -> impl std::future::Future<Output = ScopeCheck> + Send;

    fn design_fix(
        &mut self,
        ctx: &TreeContext,
        failure_reason: &str,
        round: u32,
        model: Model,
    ) -> impl std::future::Future<
        Output = Result<Result<Vec<SubtaskSpec>, String>, OrchestratorError>,
    > + Send;

    fn handle_checkpoint(
        &mut self,
        ctx: &TreeContext,
        discoveries: &[String],
    ) -> impl std::future::Future<Output = Result<ChildResponse, OrchestratorError>> + Send;

    fn can_attempt_recovery(&self, limits: &LimitsConfig) -> RecoveryEligibility;

    fn assess_and_design_recovery(
        &mut self,
        ctx: &TreeContext,
        failure_reason: &str,
        round: u32,
    ) -> impl std::future::Future<Output = Result<RecoveryDecision, OrchestratorError>> + Send;

    /// Perform assessment to determine leaf vs branch path. Called when
    /// `forced_assessment()` returns `None`.
    fn assess(
        &mut self,
        ctx: &TreeContext,
    ) -> impl std::future::Future<Output = Result<AssessmentResult, OrchestratorError>> + Send;

    /// Design decomposition and produce subtask specs for branch tasks.
    fn decompose(
        &mut self,
        ctx: &TreeContext,
        model: Model,
    ) -> impl std::future::Future<Output = Result<DecompositionResult, OrchestratorError>> + Send;
}

/// Task creation, storage, lookup, cross-task queries.
/// Implemented by the application crate's concrete state type.
pub trait TaskStore: Send {
    type Task: TaskNode;

    fn get(&self, id: TaskId) -> Option<&Self::Task>;
    fn get_mut(&mut self, id: TaskId) -> Option<&mut Self::Task>;
    fn task_count(&self) -> usize;
    fn dfs_order(&self, root: TaskId) -> Vec<TaskId>;
    fn set_root_id(&mut self, id: TaskId);
    fn save(&self, path: &Path) -> anyhow::Result<()>;

    /// Re-inject non-serializable runtime deps into all tasks after deserialization.
    fn bind_runtime(&mut self);

    /// Create a subtask under the given parent, returning the new task ID.
    ///
    /// # Panics
    ///
    /// Implementations are expected to panic if `parent_id` is not present in the store;
    /// this is treated as a programmer-error / store-invariant violation, not a recoverable condition.
    fn create_subtask(
        &mut self,
        parent_id: TaskId,
        spec: &SubtaskSpec,
        mark_fix: bool,
        inherit_recovery_rounds: Option<u32>,
    ) -> TaskId;

    /// Check if any non-fix child of the given parent completed successfully.
    fn any_non_fix_child_succeeded(&self, parent_id: TaskId) -> bool;

    /// Build a tree context snapshot for the given task.
    fn build_tree_context(&self, id: TaskId) -> Result<TreeContext, OrchestratorError>;
}
