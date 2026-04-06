// Recursive task execution, DFS traversal, state persistence, resume.
// Generic over S: TaskStore.

use crate::config::LimitsConfig;
use crate::events::{Event, EventSender};
use crate::traits::{TaskNode, TaskStore};
use crate::types::{
    BranchVerifyOutcome, FixBudgetCheck, RecoveryDecision, RecoveryEligibility, ResumePoint,
    ScopeCheck, SubtaskSpec, TaskId, TaskOutcome, TaskPath, TaskPhase,
};
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

pub struct Orchestrator<S: TaskStore> {
    store: S,
    events: EventSender,
    limits: LimitsConfig,
    state_path: Option<PathBuf>,
}

impl<S: TaskStore> Orchestrator<S> {
    pub fn new(store: S, events: EventSender) -> Self {
        Self {
            store,
            events,
            limits: LimitsConfig::default(),
            state_path: None,
        }
    }

    #[must_use]
    pub fn with_limits(mut self, mut limits: LimitsConfig) -> Self {
        // Clamp minimum values to 1 to prevent zero-iteration loops.
        limits.retry_budget = limits.retry_budget.max(1);
        limits.branch_fix_rounds = limits.branch_fix_rounds.max(1);
        limits.root_fix_rounds = limits.root_fix_rounds.max(1);
        limits.max_total_tasks = limits.max_total_tasks.max(1);
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn with_state_path(mut self, path: PathBuf) -> Self {
        self.state_path = Some(path);
        self
    }

    /// Access the underlying store (for post-run inspection).
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// Mutable access to the underlying store.
    pub const fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Consume the orchestrator and return the underlying store.
    pub fn into_store(self) -> S {
        self.store
    }

    /// Access limits config.
    pub const fn limits(&self) -> &LimitsConfig {
        &self.limits
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn checkpoint_save(&self) {
        if let Some(ref path) = self.state_path {
            if let Err(e) = self.store.save(path) {
                eprintln!("warning: state checkpoint failed: {e}");
            }
        }
    }

    fn transition(&mut self, id: TaskId, phase: TaskPhase) -> Result<(), OrchestratorError> {
        let task = self
            .store
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        task.set_phase(phase);
        self.emit(Event::PhaseTransition { task_id: id, phase });
        Ok(())
    }

    fn fail_task(&mut self, id: TaskId, reason: String) -> Result<TaskOutcome, OrchestratorError> {
        self.transition(id, TaskPhase::Failed)?;
        let outcome = TaskOutcome::Failed { reason };
        self.emit(Event::TaskCompleted {
            task_id: id,
            outcome: outcome.clone(),
        });
        self.checkpoint_save();
        Ok(outcome)
    }

    fn complete_or_fail(
        &mut self,
        id: TaskId,
        outcome: TaskOutcome,
    ) -> Result<TaskOutcome, OrchestratorError> {
        match outcome {
            TaskOutcome::Success => self.complete_task_verified(id),
            TaskOutcome::Failed { reason } => self.fail_task(id, reason),
        }
    }

    fn complete_task_verified(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        self.transition(id, TaskPhase::Completed)?;
        self.emit(Event::TaskCompleted {
            task_id: id,
            outcome: TaskOutcome::Success,
        });
        self.checkpoint_save();
        Ok(TaskOutcome::Success)
    }

    #[must_use]
    fn check_task_limit(&self, parent_id: TaskId, count: usize) -> Option<String> {
        let current = self.store.task_count();
        let max = self.limits.max_total_tasks as usize;
        if current + count > max {
            self.emit(Event::TaskLimitReached { task_id: parent_id });
            Some(format!(
                "task limit reached ({current} tasks, max {})",
                self.limits.max_total_tasks
            ))
        } else {
            None
        }
    }

    fn create_subtasks(
        &mut self,
        parent_id: TaskId,
        specs: &[SubtaskSpec],
        mark_fix: bool,
        append: bool,
        inherit_recovery_rounds: Option<u32>,
    ) -> Result<Vec<TaskId>, OrchestratorError> {
        let mut child_ids = Vec::new();
        for spec in specs {
            let child_id =
                self.store
                    .create_subtask(parent_id, spec, mark_fix, inherit_recovery_rounds);
            child_ids.push(child_id);
        }

        {
            let task = self
                .store
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            if append {
                let mut existing = task.subtask_ids().to_vec();
                existing.extend_from_slice(&child_ids);
                task.set_subtask_ids(&existing, false);
            } else {
                task.set_subtask_ids(&child_ids, false);
            }
        }

        for &child_id in &child_ids {
            if let Some(child) = self.store.get(child_id) {
                let info = child.registration_info();
                self.emit(Event::TaskRegistered {
                    task_id: child_id,
                    parent_id: info.parent_id,
                    goal: info.goal,
                    depth: info.depth,
                });
            }
        }
        self.checkpoint_save();

        Ok(child_ids)
    }

    pub async fn run(&mut self, root_id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        self.store.set_root_id(root_id);
        // Register all tasks for TUI (root + any pre-existing subtasks on resume).
        for id in self.store.dfs_order(root_id) {
            if let Some(t) = self.store.get(id) {
                let info = t.registration_info();
                self.emit(Event::TaskRegistered {
                    task_id: id,
                    parent_id: info.parent_id,
                    goal: info.goal,
                    depth: info.depth,
                });
                if info.phase != TaskPhase::Pending {
                    self.emit(Event::PhaseTransition {
                        task_id: id,
                        phase: info.phase,
                    });
                }
            }
        }
        self.execute_task(root_id).await
    }

    fn execute_task(
        &mut self,
        id: TaskId,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutcome, OrchestratorError>> + Send + '_>> {
        Box::pin(async move {
            // Resume: check where this task should re-enter.
            {
                let task = self
                    .store
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.resume_point() {
                    ResumePoint::Terminal(outcome) => return Ok(outcome),
                    ResumePoint::LeafVerifying => return self.run_leaf(id).await,
                    ResumePoint::BranchVerifying => {
                        return self.finalize_branch(id, TaskOutcome::Success).await;
                    }
                    ResumePoint::LeafExecuting => {
                        self.transition(id, TaskPhase::Executing)?;
                        return self.run_leaf(id).await;
                    }
                    ResumePoint::BranchExecuting => {
                        self.transition(id, TaskPhase::Executing)?;
                        let outcome = self.execute_branch(id).await?;
                        return self.finalize_branch(id, outcome).await;
                    }
                    ResumePoint::NeedAssessment => {}
                }
            }

            self.transition(id, TaskPhase::Assessing)?;

            let forced = {
                let task = self
                    .store
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.forced_assessment(self.limits.max_depth)
            };
            let assessment = if let Some(forced) = forced {
                forced
            } else {
                let ctx = self.store.build_tree_context(id)?;
                let task = self
                    .store
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.assess(&ctx).await?
            };

            // Apply assessment to task.
            {
                let task = self
                    .store
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
            self.checkpoint_save();

            self.transition(id, TaskPhase::Executing)?;

            match assessment.path {
                TaskPath::Leaf => self.run_leaf(id).await,
                TaskPath::Branch => {
                    let outcome = self.execute_branch(id).await?;
                    self.finalize_branch(id, outcome).await
                }
            }
        })
    }

    async fn run_leaf(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        let tree = self.store.build_tree_context(id)?;
        let task = self
            .store
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        let outcome = task.execute_leaf(&tree).await;
        // Agent-level errors propagated as infrastructure failures.
        if let TaskOutcome::Failed { ref reason } = outcome {
            if let Some(msg) = reason.strip_prefix("__agent_error__: ") {
                return Err(OrchestratorError::Agent(anyhow::anyhow!("{msg}")));
            }
        }
        self.complete_or_fail(id, outcome)
    }

    async fn finalize_branch(
        &mut self,
        id: TaskId,
        outcome: TaskOutcome,
    ) -> Result<TaskOutcome, OrchestratorError> {
        if outcome == TaskOutcome::Success {
            self.transition(id, TaskPhase::Verifying)?;

            let tree = self.store.build_tree_context(id)?;
            let task = self
                .store
                .get_mut(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            let verify_outcome = task.verify_branch(&tree).await?;

            match verify_outcome {
                BranchVerifyOutcome::Passed => self.complete_task_verified(id),
                BranchVerifyOutcome::FailedNoFixLoop { reason } => self.fail_task(id, reason),
                BranchVerifyOutcome::Failed { reason } => self.branch_fix_loop(id, &reason).await,
            }
        } else {
            let TaskOutcome::Failed { reason } = outcome else {
                unreachable!("non-Success outcome must be Failed");
            };
            self.fail_task(id, reason)
        }
    }

    async fn branch_fix_loop(
        &mut self,
        id: TaskId,
        initial_failure: &str,
    ) -> Result<TaskOutcome, OrchestratorError> {
        let mut failure_reason = initial_failure.to_owned();

        loop {
            let model = {
                let task = self
                    .store
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.fix_round_budget_check(&self.limits) {
                    FixBudgetCheck::Exhausted => {
                        return self.fail_task(id, failure_reason);
                    }
                    FixBudgetCheck::WithinBudget { model } => model,
                }
            };

            let round = {
                let task = self
                    .store
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.increment_fix_rounds()
            };

            // Scope circuit breaker.
            {
                let task = self
                    .store
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.check_branch_scope().await {
                    ScopeCheck::WithinBounds => {}
                    ScopeCheck::Exceeded {
                        metric,
                        actual,
                        limit,
                    } => {
                        return self.fail_task(
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

            // Design fix subtasks.
            let tree = self.store.build_tree_context(id)?;
            let specs = {
                let task = self
                    .store
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.design_fix(&tree, &failure_reason, round, model)
                    .await?
            };

            let subtask_specs = match specs {
                Ok(s) => s,
                Err(reason) => {
                    failure_reason = reason;
                    self.checkpoint_save();
                    continue;
                }
            };

            if let Some(reason) = self.check_task_limit(id, subtask_specs.len()) {
                return self.fail_task(id, reason);
            }

            let fix_child_ids = self.create_subtasks(id, &subtask_specs, true, true, None)?;
            self.emit(Event::FixSubtasksCreated {
                task_id: id,
                count: fix_child_ids.len(),
                round,
            });

            for &child_id in &fix_child_ids {
                let _child_outcome = self.execute_task(child_id).await?;
            }

            // Re-verify the branch after fix subtasks complete.
            let tree = self.store.build_tree_context(id)?;
            let task = self
                .store
                .get_mut(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            let verify_result = match task.verify_branch(&tree).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("warning: verify failed: {e}");
                    BranchVerifyOutcome::Failed {
                        reason: format!("verification error: {e}"),
                    }
                }
            };
            match verify_result {
                BranchVerifyOutcome::Passed => return self.complete_task_verified(id),
                BranchVerifyOutcome::Failed { reason }
                | BranchVerifyOutcome::FailedNoFixLoop { reason } => {
                    failure_reason = reason;
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_branch(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        let (should_decompose, decompose_model) = {
            let task = self
                .store
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            (task.needs_decomposition(), task.decompose_model())
        };

        if should_decompose {
            let ctx = self.store.build_tree_context(id)?;
            let task = self
                .store
                .get_mut(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            let decomposition = task.decompose(&ctx, decompose_model).await?;
            task.set_decomposition_rationale(decomposition.rationale);

            if let Some(reason) = self.check_task_limit(id, decomposition.subtasks.len()) {
                return Ok(TaskOutcome::Failed { reason });
            }

            let new_child_ids =
                self.create_subtasks(id, &decomposition.subtasks, false, false, None)?;
            self.emit(Event::SubtasksCreated {
                parent_id: id,
                child_ids: new_child_ids,
            });
        }

        // Outer loop: restarts child iteration after recovery creates new subtasks.
        loop {
            let child_ids = self
                .store
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .subtask_ids()
                .to_vec();

            let mut all_done = true;

            for &child_id in &child_ids {
                let child = self
                    .store
                    .get(child_id)
                    .ok_or(OrchestratorError::TaskNotFound(child_id))?;

                if child.is_terminal() {
                    continue;
                }

                all_done = false;
                let child_outcome = self.execute_task(child_id).await?;

                // Check for discoveries.
                let child_discoveries = self
                    .store
                    .get(child_id)
                    .ok_or(OrchestratorError::TaskNotFound(child_id))?
                    .discoveries()
                    .to_vec();

                if !child_discoveries.is_empty() {
                    let tree = self.store.build_tree_context(id)?;
                    let response = {
                        let task = self
                            .store
                            .get_mut(id)
                            .ok_or(OrchestratorError::TaskNotFound(id))?;
                        task.handle_checkpoint(&tree, &child_discoveries).await?
                    };
                    self.checkpoint_save();

                    match response {
                        crate::types::ChildResponse::Continue => {}
                        crate::types::ChildResponse::NeedRecoverySubtasks {
                            specs,
                            supersede_pending,
                        } => {
                            if supersede_pending {
                                let existing_child_ids = self
                                    .store
                                    .get(id)
                                    .ok_or(OrchestratorError::TaskNotFound(id))?
                                    .subtask_ids()
                                    .to_vec();
                                for &eid in &existing_child_ids {
                                    let existing_child = self
                                        .store
                                        .get_mut(eid)
                                        .ok_or(OrchestratorError::TaskNotFound(eid))?;
                                    if existing_child.phase() == TaskPhase::Pending {
                                        existing_child.set_phase(TaskPhase::Failed);
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
                            if let Some(msg) = self.check_task_limit(id, specs.len()) {
                                return Ok(TaskOutcome::Failed {
                                    reason: format!("{msg}: checkpoint escalation"),
                                });
                            }
                            let count = specs.len();
                            let parent_rounds = self
                                .store
                                .get(id)
                                .ok_or(OrchestratorError::TaskNotFound(id))?
                                .recovery_rounds();
                            self.create_subtasks(id, &specs, false, true, Some(parent_rounds))?;
                            self.emit(Event::RecoverySubtasksCreated {
                                task_id: id,
                                count,
                                round: parent_rounds,
                            });
                            break;
                        }
                        crate::types::ChildResponse::Failed(reason) => {
                            return Ok(TaskOutcome::Failed { reason });
                        }
                    }
                }

                if let TaskOutcome::Failed { ref reason } = child_outcome {
                    if let Some(recovery_outcome) = self.attempt_recovery(id, reason).await? {
                        return Ok(recovery_outcome);
                    }
                    break;
                }
            }

            if all_done {
                break;
            }
        }

        if !self.store.any_non_fix_child_succeeded(id) {
            return Ok(TaskOutcome::Failed {
                reason: "all non-fix children failed".into(),
            });
        }

        Ok(TaskOutcome::Success)
    }

    async fn attempt_recovery(
        &mut self,
        parent_id: TaskId,
        failure_reason: &str,
    ) -> Result<Option<TaskOutcome>, OrchestratorError> {
        let task = self
            .store
            .get(parent_id)
            .ok_or(OrchestratorError::TaskNotFound(parent_id))?;

        let round = match task.can_attempt_recovery(&self.limits) {
            RecoveryEligibility::NotEligible { reason } => {
                return Ok(Some(TaskOutcome::Failed {
                    reason: format!("{reason}: {failure_reason}"),
                }));
            }
            RecoveryEligibility::Eligible { round } => round,
        };

        let tree = self.store.build_tree_context(parent_id)?;
        let decision = {
            let task = self
                .store
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            task.assess_and_design_recovery(&tree, failure_reason, round)
                .await?
        };
        self.checkpoint_save();

        match decision {
            RecoveryDecision::Unrecoverable { reason } => Ok(Some(TaskOutcome::Failed { reason })),
            RecoveryDecision::Plan {
                specs,
                supersede_pending,
            } => {
                if supersede_pending {
                    let child_ids = self
                        .store
                        .get(parent_id)
                        .ok_or(OrchestratorError::TaskNotFound(parent_id))?
                        .subtask_ids()
                        .to_vec();

                    for &child_id in &child_ids {
                        let child = self
                            .store
                            .get_mut(child_id)
                            .ok_or(OrchestratorError::TaskNotFound(child_id))?;
                        if child.phase() == TaskPhase::Pending {
                            child.set_phase(TaskPhase::Failed);
                            self.emit(Event::TaskCompleted {
                                task_id: child_id,
                                outcome: TaskOutcome::Failed {
                                    reason: "superseded by recovery re-decomposition".into(),
                                },
                            });
                        }
                    }
                }

                if let Some(msg) = self.check_task_limit(parent_id, specs.len()) {
                    return Ok(Some(TaskOutcome::Failed {
                        reason: format!("{msg}: {failure_reason}"),
                    }));
                }

                let count = specs.len();
                let parent_rounds = self
                    .store
                    .get(parent_id)
                    .ok_or(OrchestratorError::TaskNotFound(parent_id))?
                    .recovery_rounds();
                self.create_subtasks(parent_id, &specs, false, true, Some(parent_rounds))?;

                self.emit(Event::RecoverySubtasksCreated {
                    task_id: parent_id,
                    count,
                    round,
                });

                Ok(None)
            }
        }
    }
}
