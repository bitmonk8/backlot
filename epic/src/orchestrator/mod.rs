// Recursive task execution, DFS traversal, state persistence, resume.

pub mod context;
pub mod services;

use crate::agent::{AgentService, SessionMeta, TaskContext};
use crate::config::project::LimitsConfig;
use crate::events::{Event, EventSender};
use crate::state::EpicState;
use crate::task::assess::AssessmentResult;
use crate::task::branch::SubtaskSpec;
use crate::task::scope::ScopeCheck;
use crate::task::verify::{VerificationOutcome, VerifyOutcome};
use crate::task::{Model, Task, TaskId, TaskOutcome, TaskPath, TaskPhase};
use services::Services;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("task not found: {0}")]
    TaskNotFound(TaskId),
    #[error("agent error: {0}")]
    Agent(#[from] anyhow::Error),
}

pub struct Orchestrator<A: AgentService> {
    services: Services<A>,
}

impl<A: AgentService> Orchestrator<A> {
    pub fn new(agent: A, events: EventSender) -> Self {
        Self {
            services: Services {
                agent,
                events,
                vault: None,
                limits: LimitsConfig::default(),
                project_root: None,
                state_path: None,
            },
        }
    }

    pub fn with_limits(mut self, mut limits: LimitsConfig) -> Self {
        // Clamp minimum values to 1 to prevent zero-iteration loops.
        limits.retry_budget = limits.retry_budget.max(1);
        limits.branch_fix_rounds = limits.branch_fix_rounds.max(1);
        limits.root_fix_rounds = limits.root_fix_rounds.max(1);
        limits.max_total_tasks = limits.max_total_tasks.max(1);
        self.services.limits = limits;
        self
    }

    pub fn with_state_path(mut self, path: PathBuf) -> Self {
        self.services.state_path = Some(path);
        self
    }

    pub fn with_project_root(mut self, path: PathBuf) -> Self {
        self.services.project_root = Some(path);
        self
    }

    pub fn with_vault(mut self, vault: std::sync::Arc<vault::Vault>) -> Self {
        self.services.vault = Some(vault);
        self
    }

    fn accumulate_usage(&self, state: &mut EpicState, task_id: TaskId, meta: &SessionMeta) {
        let total_cost = if let Some(task) = state.get_mut(task_id) {
            task.accumulate_usage(meta);
            Some(task.usage.cost_usd)
        } else {
            None
        };
        if let Some(total_cost_usd) = total_cost {
            self.emit(Event::UsageUpdated {
                task_id,
                phase_cost_usd: meta.cost_usd,
                total_cost_usd,
            });
        }
    }

    /// Record content to vault (best-effort). Errors are logged, not propagated.
    async fn record_to_vault(
        &self,
        state: &mut EpicState,
        task_id: TaskId,
        name: &str,
        content: &str,
    ) {
        let Some(ref vault) = self.services.vault else {
            return;
        };
        let result = match vault.record(name, content, vault::RecordMode::New).await {
            Err(vault::RecordError::VersionConflict(_)) => {
                vault.record(name, content, vault::RecordMode::Append).await
            }
            other => other,
        };
        match result {
            Ok((_refs, _warnings, meta)) => {
                let session_meta = SessionMeta::from_vault(&meta);
                self.accumulate_usage(state, task_id, &session_meta);
                self.emit(Event::VaultRecorded {
                    task_id,
                    document: name.to_string(),
                });
            }
            Err(e) => {
                eprintln!("warning: vault record failed for {name}: {e}");
            }
        }
    }

    /// Reorganize vault documents (best-effort). Errors are logged, not propagated.
    async fn reorganize_vault(&self, state: &mut EpicState, task_id: TaskId) {
        let Some(ref vault) = self.services.vault else {
            return;
        };
        match vault.reorganize().await {
            Ok((report, _warnings, meta)) => {
                let session_meta = SessionMeta::from_vault(&meta);
                self.accumulate_usage(state, task_id, &session_meta);
                self.emit(Event::VaultReorganizeCompleted {
                    merged: report.merged.len(),
                    restructured: report.restructured.len(),
                    deleted: report.deleted.len(),
                });
            }
            Err(e) => {
                eprintln!("warning: vault reorganize failed: {e}");
            }
        }
    }

    pub async fn run(
        &self,
        state: &mut EpicState,
        root_id: TaskId,
    ) -> Result<TaskOutcome, OrchestratorError> {
        state.set_root_id(root_id);
        // Register all tasks for TUI (root + any pre-existing subtasks on resume).
        for id in state.dfs_order(root_id) {
            if let Some(t) = state.get(id) {
                self.emit(Event::TaskRegistered {
                    task_id: id,
                    parent_id: t.parent_id,
                    goal: t.goal.clone(),
                    depth: t.depth,
                });
                if t.phase != TaskPhase::Pending {
                    self.emit(Event::PhaseTransition {
                        task_id: id,
                        phase: t.phase,
                    });
                }
            }
        }
        self.execute_task(state, root_id).await
    }

    /// Write state to disk if a state path is configured. Best-effort: logs
    /// but does not propagate write errors to avoid aborting the run.
    fn checkpoint_save(&self, state: &EpicState) {
        if let Some(ref path) = self.services.state_path {
            if let Err(e) = state.save(path) {
                eprintln!("warning: state checkpoint failed: {e}");
            }
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.services.events.send(event);
    }

    fn transition(
        &self,
        state: &mut EpicState,
        id: TaskId,
        phase: TaskPhase,
    ) -> Result<(), OrchestratorError> {
        let task = state
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        task.phase = phase;
        self.emit(Event::PhaseTransition { task_id: id, phase });
        Ok(())
    }

    fn fail_task(
        &self,
        state: &mut EpicState,
        id: TaskId,
        reason: String,
    ) -> Result<TaskOutcome, OrchestratorError> {
        self.transition(state, id, TaskPhase::Failed)?;
        let outcome = TaskOutcome::Failed { reason };
        self.emit(Event::TaskCompleted {
            task_id: id,
            outcome: outcome.clone(),
        });
        self.checkpoint_save(state);
        Ok(outcome)
    }

    fn complete_or_fail(
        &self,
        state: &mut EpicState,
        id: TaskId,
        outcome: TaskOutcome,
    ) -> Result<TaskOutcome, OrchestratorError> {
        match outcome {
            TaskOutcome::Success => self.complete_task_verified(state, id),
            TaskOutcome::Failed { reason } => self.fail_task(state, id, reason),
        }
    }

    fn complete_task_verified(
        &self,
        state: &mut EpicState,
        id: TaskId,
    ) -> Result<TaskOutcome, OrchestratorError> {
        self.transition(state, id, TaskPhase::Completed)?;
        self.emit(Event::TaskCompleted {
            task_id: id,
            outcome: TaskOutcome::Success,
        });
        self.checkpoint_save(state);
        Ok(TaskOutcome::Success)
    }

    async fn try_verify(
        &self,
        state: &mut EpicState,
        id: TaskId,
    ) -> Result<VerifyOutcome, OrchestratorError> {
        let verify_model = self.verification_model(state, id)?;
        let ctx = self.build_context(state, id)?;
        match self.services.agent.verify(&ctx, verify_model).await {
            Ok(agent_result) => {
                self.accumulate_usage(state, id, &agent_result.meta);
                match agent_result.value.outcome {
                    VerificationOutcome::Pass => {
                        if let Some(fail_reason) = self.try_file_level_review(state, id).await? {
                            Ok(VerifyOutcome::Failed(fail_reason))
                        } else {
                            self.complete_task_verified(state, id)?;
                            Ok(VerifyOutcome::Passed)
                        }
                    }
                    VerificationOutcome::Fail { reason } => {
                        self.record_to_vault(state, id, "VERIFICATION_FAILURE", &reason)
                            .await;
                        self.checkpoint_save(state);
                        Ok(VerifyOutcome::Failed(reason))
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: verify failed: {e}");
                self.checkpoint_save(state);
                Ok(VerifyOutcome::Failed(format!("verification error: {e}")))
            }
        }
    }

    /// Run file-level review for leaf tasks after verification passes.
    /// Returns `Some(reason)` on failure, `None` on pass or skip (non-leaf).
    async fn try_file_level_review(
        &self,
        state: &mut EpicState,
        id: TaskId,
    ) -> Result<Option<String>, OrchestratorError> {
        let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
        if task.path != Some(TaskPath::Leaf) {
            return Ok(None);
        }

        let review_model = self.verification_model(state, id)?;
        let review_ctx = self.build_context(state, id)?;
        let review_result = self
            .services
            .agent
            .file_level_review(&review_ctx, review_model)
            .await?;
        self.accumulate_usage(state, id, &review_result.meta);

        let passed = review_result.value.outcome == VerificationOutcome::Pass;
        self.emit(Event::FileLevelReviewCompleted {
            task_id: id,
            passed,
        });

        match review_result.value.outcome {
            VerificationOutcome::Pass => Ok(None),
            VerificationOutcome::Fail { reason } => Ok(Some(reason)),
        }
    }

    /// If creating `count` new tasks would exceed the global limit, emits
    /// `TaskLimitReached` and returns `Some(reason_string)`. Otherwise returns `None`.
    #[must_use]
    fn check_task_limit(
        &self,
        state: &EpicState,
        parent_id: TaskId,
        count: usize,
    ) -> Option<String> {
        let current = state.task_count();
        let max = self.services.limits.max_total_tasks as usize;
        if current + count > max {
            self.emit(Event::TaskLimitReached { task_id: parent_id });
            Some(format!(
                "task limit reached ({current} tasks, max {})",
                self.services.limits.max_total_tasks
            ))
        } else {
            None
        }
    }

    fn create_subtasks(
        &self,
        state: &mut EpicState,
        parent_id: TaskId,
        specs: Vec<SubtaskSpec>,
        mark_fix: bool,
        append: bool,
        inherit_recovery_rounds: Option<u32>,
    ) -> Result<Vec<TaskId>, OrchestratorError> {
        let parent_depth = state
            .get(parent_id)
            .ok_or(OrchestratorError::TaskNotFound(parent_id))?
            .depth;

        let mut child_ids = Vec::new();
        for spec in specs {
            let child_id = state.next_task_id();
            let mut child = Task::new(
                child_id,
                Some(parent_id),
                spec.goal,
                spec.verification_criteria,
                parent_depth + 1,
            );
            child.magnitude_estimate = Some(spec.magnitude_estimate);
            child.is_fix_task = mark_fix;
            if let Some(rounds) = inherit_recovery_rounds {
                child.recovery_rounds = rounds;
            }
            child_ids.push(child_id);
            state.insert(child);
        }

        {
            let task = state
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            if append {
                task.subtask_ids.extend_from_slice(&child_ids);
            } else {
                task.subtask_ids.clone_from(&child_ids);
            }
        }

        for &child_id in &child_ids {
            if let Some(child) = state.get(child_id) {
                self.emit(Event::TaskRegistered {
                    task_id: child_id,
                    parent_id: child.parent_id,
                    goal: child.goal.clone(),
                    depth: child.depth,
                });
            }
        }
        self.checkpoint_save(state);

        Ok(child_ids)
    }

    #[allow(clippy::unused_self)]
    fn build_context(
        &self,
        state: &EpicState,
        id: TaskId,
    ) -> Result<TaskContext, OrchestratorError> {
        context::build_context(state, id)
    }

    // Returns boxed future to support recursion (execute_task → execute_branch → execute_task).
    fn execute_task<'a>(
        &'a self,
        state: &'a mut EpicState,
        id: TaskId,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutcome, OrchestratorError>> + Send + 'a>> {
        Box::pin(async move {
            // Resume: skip already-terminal tasks.
            {
                let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.phase {
                    TaskPhase::Completed => return Ok(TaskOutcome::Success),
                    TaskPhase::Failed => {
                        return Ok(TaskOutcome::Failed {
                            reason: "previously failed".into(),
                        });
                    }
                    _ => {}
                }
            }

            // Resume: task was mid-verification or mid-execution with path set.
            {
                let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
                let path = task.path.clone();
                let phase = task.phase;

                if let (Some(p), TaskPhase::Verifying) = (&path, phase) {
                    return match p {
                        TaskPath::Leaf => self.run_leaf(state, id).await,
                        TaskPath::Branch => {
                            self.finalize_branch(state, id, TaskOutcome::Success).await
                        }
                    };
                }

                if let (Some(p), TaskPhase::Executing) = (&path, phase) {
                    let p = p.clone();
                    self.transition(state, id, TaskPhase::Executing)?;
                    return match p {
                        TaskPath::Leaf => self.run_leaf(state, id).await,
                        TaskPath::Branch => {
                            let outcome = self.execute_branch(state, id).await?;
                            self.finalize_branch(state, id, outcome).await
                        }
                    };
                }
            }

            self.transition(state, id, TaskPhase::Assessing)?;

            let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
            let is_root = task.parent_id.is_none();
            let depth = task.depth;

            let assessment = if is_root {
                AssessmentResult {
                    path: TaskPath::Branch,
                    model: Model::Sonnet,
                    rationale: "Root task always branches".into(),
                    magnitude: None,
                }
            } else if depth >= self.services.limits.max_depth {
                AssessmentResult {
                    path: TaskPath::Leaf,
                    model: Model::Sonnet,
                    rationale: "Depth cap reached, forced to leaf".into(),
                    magnitude: None,
                }
            } else {
                let ctx = self.build_context(state, id)?;
                let agent_result = self.services.agent.assess(&ctx).await?;
                self.accumulate_usage(state, id, &agent_result.meta);
                agent_result.value
            };

            // Apply assessment to task.
            {
                let task = state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.set_assessment(
                    assessment.path.clone(),
                    assessment.model,
                    assessment.magnitude.clone(),
                );
            }

            self.emit(Event::PathSelected {
                task_id: id,
                path: assessment.path.clone(),
            });
            self.emit(Event::ModelSelected {
                task_id: id,
                model: assessment.model,
            });
            self.checkpoint_save(state);

            self.transition(state, id, TaskPhase::Executing)?;

            match assessment.path {
                TaskPath::Leaf => self.run_leaf(state, id).await,
                TaskPath::Branch => {
                    let outcome = self.execute_branch(state, id).await?;
                    self.finalize_branch(state, id, outcome).await
                }
            }
        })
    }

    #[allow(clippy::unused_self)]
    fn verification_model(
        &self,
        state: &EpicState,
        id: TaskId,
    ) -> Result<Model, OrchestratorError> {
        let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
        Ok(task.verification_model())
    }

    /// Branch-only finalization: verify + fix loop. Leaf tasks use `Task::execute_leaf`.
    async fn finalize_branch(
        &self,
        state: &mut EpicState,
        id: TaskId,
        outcome: TaskOutcome,
    ) -> Result<TaskOutcome, OrchestratorError> {
        if outcome == TaskOutcome::Success {
            self.transition(state, id, TaskPhase::Verifying)?;

            let tree = context::build_tree_context(state, id)?;
            let task = state
                .get_mut(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            let verify_outcome = task.verify_branch(&tree, &self.services).await?;
            let is_fix_task = task.is_fix_task;

            match verify_outcome {
                crate::task::branch::BranchVerifyOutcome::Passed => {
                    self.complete_task_verified(state, id)
                }
                crate::task::branch::BranchVerifyOutcome::Failed { reason } => {
                    if is_fix_task {
                        self.fail_task(state, id, reason)
                    } else {
                        self.branch_fix_loop(state, id, &reason).await
                    }
                }
            }
        } else {
            // outcome is already Failed; extract reason for fail_task helper.
            let TaskOutcome::Failed { reason } = outcome else {
                unreachable!("non-Success outcome must be Failed");
            };
            self.fail_task(state, id, reason)
        }
    }

    async fn branch_fix_loop(
        &self,
        state: &mut EpicState,
        id: TaskId,
        initial_failure: &str,
    ) -> Result<TaskOutcome, OrchestratorError> {
        let is_root = state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .parent_id
            .is_none();

        let mut failure_reason = initial_failure.to_owned();

        loop {
            // Check round budget via Task method.
            let model = {
                let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.fix_round_budget_check(is_root, &self.services.limits) {
                    crate::task::branch::FixBudgetCheck::Exhausted => {
                        return self.fail_task(state, id, failure_reason);
                    }
                    crate::task::branch::FixBudgetCheck::WithinBudget { model } => model,
                }
            };

            let round = {
                let task = state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.increment_fix_rounds()
            };

            // Scope circuit breaker via Task method.
            {
                let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.check_branch_scope(&self.services).await {
                    ScopeCheck::WithinBounds => {}
                    ScopeCheck::Exceeded {
                        metric,
                        actual,
                        limit,
                    } => {
                        return self.fail_task(
                            state,
                            id,
                            format!("SCOPE_EXCEEDED: {metric} actual={actual} limit={limit}"),
                        );
                    }
                }
            }

            self.emit(Event::BranchFixRound {
                task_id: id,
                round,
                model,
            });

            // Design fix subtasks via Task method.
            let tree = context::build_tree_context(state, id)?;
            let specs = {
                let task = state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.design_fix(&tree, &self.services, &failure_reason, round, model)
                    .await?
            };

            let subtask_specs = match specs {
                Ok(s) => s,
                Err(reason) => {
                    failure_reason = reason;
                    self.checkpoint_save(state);
                    continue;
                }
            };

            if let Some(reason) = self.check_task_limit(state, id, subtask_specs.len()) {
                return self.fail_task(state, id, reason);
            }

            // Cross-task: create subtasks (stays in orchestrator).
            let fix_child_ids = self.create_subtasks(state, id, subtask_specs, true, true, None)?;
            self.emit(Event::FixSubtasksCreated {
                task_id: id,
                count: fix_child_ids.len(),
                round,
            });

            // Cross-task: execute each fix subtask (stays in orchestrator).
            for &child_id in &fix_child_ids {
                let _child_outcome = self.execute_task(state, child_id).await?;
            }

            // Re-verify the branch after fix subtasks complete.
            match self.try_verify(state, id).await? {
                VerifyOutcome::Passed => return Ok(TaskOutcome::Success),
                VerifyOutcome::Failed(reason) => failure_reason = reason,
            }
        }
    }

    async fn run_leaf(
        &self,
        state: &mut EpicState,
        id: TaskId,
    ) -> Result<TaskOutcome, OrchestratorError> {
        let tree = context::build_tree_context(state, id)?;
        let task = state
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        let outcome = task.execute_leaf(&tree, &self.services).await;
        // Agent-level errors propagated as infrastructure failures.
        if let TaskOutcome::Failed { ref reason } = outcome {
            if let Some(msg) = reason.strip_prefix("__agent_error__: ") {
                return Err(OrchestratorError::Agent(anyhow::anyhow!("{msg}")));
            }
        }
        self.complete_or_fail(state, id, outcome)
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_branch(
        &self,
        state: &mut EpicState,
        id: TaskId,
    ) -> Result<TaskOutcome, OrchestratorError> {
        // Resume: reuse existing subtasks if already decomposed.
        let existing_subtasks = state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .subtask_ids
            .clone();

        if existing_subtasks.is_empty() {
            let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
            let decompose_model = task.current_model.unwrap_or(Model::Sonnet);
            let ctx = self.build_context(state, id)?;
            let agent_result = self
                .services
                .agent
                .design_and_decompose(&ctx, decompose_model)
                .await?;
            self.accumulate_usage(state, id, &agent_result.meta);
            let decomposition = agent_result.value;

            {
                let task = state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.set_decomposition_rationale(decomposition.rationale);
            }

            if let Some(reason) = self.check_task_limit(state, id, decomposition.subtasks.len()) {
                return Ok(TaskOutcome::Failed { reason });
            }

            let new_child_ids =
                self.create_subtasks(state, id, decomposition.subtasks, false, false, None)?;
            self.emit(Event::SubtasksCreated {
                parent_id: id,
                child_ids: new_child_ids,
            });
        }

        // Outer loop: restarts child iteration after recovery creates new subtasks.
        loop {
            let child_ids = state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .subtask_ids
                .clone();

            let mut all_done = true;

            for &child_id in &child_ids {
                let child_phase = state
                    .get(child_id)
                    .ok_or(OrchestratorError::TaskNotFound(child_id))?
                    .phase;

                match child_phase {
                    TaskPhase::Completed | TaskPhase::Failed => continue,
                    _ => {}
                }

                all_done = false;
                let child_outcome = self.execute_task(state, child_id).await?;

                // Check for discoveries → checkpoint.
                let child_discoveries = state
                    .get(child_id)
                    .ok_or(OrchestratorError::TaskNotFound(child_id))?
                    .discoveries
                    .clone();

                if !child_discoveries.is_empty() {
                    use crate::task::branch::ChildResponse;

                    let tree = context::build_tree_context(state, id)?;
                    let response = {
                        let task = state
                            .get_mut(id)
                            .ok_or(OrchestratorError::TaskNotFound(id))?;
                        task.handle_checkpoint(&tree, &self.services, &child_discoveries)
                            .await?
                    };
                    self.checkpoint_save(state);

                    match response {
                        ChildResponse::Continue => {}
                        ChildResponse::NeedRecoverySubtasks {
                            specs,
                            supersede_pending,
                        } => {
                            // Cross-task: mark pending children as Failed.
                            if supersede_pending {
                                let existing_child_ids = state
                                    .get(id)
                                    .ok_or(OrchestratorError::TaskNotFound(id))?
                                    .subtask_ids
                                    .clone();
                                for &eid in &existing_child_ids {
                                    let existing_child = state
                                        .get_mut(eid)
                                        .ok_or(OrchestratorError::TaskNotFound(eid))?;
                                    if existing_child.phase == TaskPhase::Pending {
                                        existing_child.phase = TaskPhase::Failed;
                                        self.emit(Event::TaskCompleted {
                                            task_id: eid,
                                            outcome: TaskOutcome::Failed {
                                                reason: "superseded by recovery re-decomposition"
                                                    .into(),
                                            },
                                        });
                                    }
                                }
                            }
                            // Cross-task: check task limit and create subtasks.
                            if let Some(msg) = self.check_task_limit(state, id, specs.len()) {
                                return Ok(TaskOutcome::Failed {
                                    reason: format!("{msg}: checkpoint escalation"),
                                });
                            }
                            let count = specs.len();
                            let parent_rounds = state
                                .get(id)
                                .ok_or(OrchestratorError::TaskNotFound(id))?
                                .recovery_rounds;
                            self.create_subtasks(
                                state,
                                id,
                                specs,
                                false,
                                true,
                                Some(parent_rounds),
                            )?;
                            self.emit(Event::RecoverySubtasksCreated {
                                task_id: id,
                                count,
                                round: parent_rounds,
                            });
                            // Restart child loop.
                            break;
                        }
                        ChildResponse::Failed(reason) => {
                            return Ok(TaskOutcome::Failed { reason });
                        }
                    }
                }

                if let TaskOutcome::Failed { ref reason } = child_outcome {
                    if let Some(recovery_outcome) = self.attempt_recovery(state, id, reason).await?
                    {
                        // Recovery failed or not possible — propagate failure.
                        return Ok(recovery_outcome);
                    }
                    // Recovery succeeded: new subtasks created, restart child loop.
                    break;
                }
            }

            if all_done {
                break;
            }
        }

        // Reorganize vault after all children complete (before verification).
        self.reorganize_vault(state, id).await;

        // Guard: if every non-fix child failed (recovery exhausted or skipped),
        // the branch itself must report failure rather than vacuous success.
        let child_ids = state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .subtask_ids
            .clone();
        let any_non_fix_succeeded = child_ids.iter().any(|&cid| {
            state
                .get(cid)
                .is_some_and(|c| !c.is_fix_task && c.phase == TaskPhase::Completed)
        });
        if !any_non_fix_succeeded {
            return Ok(TaskOutcome::Failed {
                reason: "all non-fix children failed".into(),
            });
        }

        Ok(TaskOutcome::Success)
    }

    /// Attempt recovery after a child failure. Returns `Some(Failed)` if recovery
    /// is not possible or rounds are exhausted. Returns `None` if recovery subtasks
    /// were created successfully (caller should restart the child loop).
    async fn attempt_recovery(
        &self,
        state: &mut EpicState,
        parent_id: TaskId,
        failure_reason: &str,
    ) -> Result<Option<TaskOutcome>, OrchestratorError> {
        use crate::task::branch::RecoveryDecision;

        let task = state
            .get(parent_id)
            .ok_or(OrchestratorError::TaskNotFound(parent_id))?;

        // No recovery for fix tasks (prevents recursive recovery chains).
        if task.is_fix_task {
            return Ok(Some(TaskOutcome::Failed {
                reason: failure_reason.to_string(),
            }));
        }

        // Check recovery round budget via Task method.
        if !task.recovery_budget_check(&self.services.limits) {
            let max_recovery = self.services.limits.max_recovery_rounds;
            return Ok(Some(TaskOutcome::Failed {
                reason: format!("recovery rounds exhausted ({max_recovery}): {failure_reason}"),
            }));
        }

        let round = task.recovery_rounds + 1;

        // assess_and_design_recovery increments recovery_rounds internally
        // after assessment succeeds but before design.
        // Checkpoint after the call since the task may have been mutated.

        // Assess and design recovery via Task method.
        let tree = context::build_tree_context(state, parent_id)?;
        let decision = {
            let task = state
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            task.assess_and_design_recovery(&tree, &self.services, failure_reason, round)
                .await?
        };
        self.checkpoint_save(state);

        match decision {
            RecoveryDecision::Unrecoverable { reason } => Ok(Some(TaskOutcome::Failed { reason })),
            RecoveryDecision::Plan {
                specs,
                supersede_pending,
            } => {
                // Cross-task: mark pending children as Failed for full re-decomposition.
                if supersede_pending {
                    let child_ids = state
                        .get(parent_id)
                        .ok_or(OrchestratorError::TaskNotFound(parent_id))?
                        .subtask_ids
                        .clone();

                    for &child_id in &child_ids {
                        let child = state
                            .get_mut(child_id)
                            .ok_or(OrchestratorError::TaskNotFound(child_id))?;
                        if child.phase == TaskPhase::Pending {
                            child.phase = TaskPhase::Failed;
                            self.emit(Event::TaskCompleted {
                                task_id: child_id,
                                outcome: TaskOutcome::Failed {
                                    reason: "superseded by recovery re-decomposition".into(),
                                },
                            });
                        }
                    }
                }

                // Cross-task: check task limit and create recovery subtasks.
                if let Some(msg) = self.check_task_limit(state, parent_id, specs.len()) {
                    return Ok(Some(TaskOutcome::Failed {
                        reason: format!("{msg}: {failure_reason}"),
                    }));
                }

                let count = specs.len();

                // Read parent's recovery round counter before creating subtasks so that
                // children inherit it during creation (prevents exponential cost growth).
                let parent_rounds = state
                    .get(parent_id)
                    .ok_or(OrchestratorError::TaskNotFound(parent_id))?
                    .recovery_rounds;
                self.create_subtasks(state, parent_id, specs, false, true, Some(parent_rounds))?;

                self.emit(Event::RecoverySubtasksCreated {
                    task_id: parent_id,
                    count,
                    round,
                });

                // Return None to signal caller should restart the child loop.
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests;
