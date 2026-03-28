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
    state: EpicState,
}

impl<A: AgentService> Orchestrator<A> {
    pub fn new(agent: A, state: EpicState, events: EventSender) -> Self {
        Self {
            services: Services {
                agent,
                events,
                vault: None,
                limits: LimitsConfig::default(),
                project_root: None,
                state_path: None,
            },
            state,
        }
    }

    pub const fn with_limits(mut self, limits: LimitsConfig) -> Self {
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

    fn accumulate_usage(&mut self, task_id: TaskId, meta: &SessionMeta) {
        let total_cost = if let Some(task) = self.state.get_mut(task_id) {
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
    async fn record_to_vault(&mut self, task_id: TaskId, name: &str, content: &str) {
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
                self.accumulate_usage(task_id, &session_meta);
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
    async fn reorganize_vault(&mut self, task_id: TaskId) {
        let Some(ref vault) = self.services.vault else {
            return;
        };
        match vault.reorganize().await {
            Ok((report, _warnings, meta)) => {
                let session_meta = SessionMeta::from_vault(&meta);
                self.accumulate_usage(task_id, &session_meta);
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

    pub async fn run(&mut self, root_id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        // Clamp minimum values to 1 to prevent zero-iteration loops.
        self.services.limits.retry_budget = self.services.limits.retry_budget.max(1);
        self.services.limits.branch_fix_rounds = self.services.limits.branch_fix_rounds.max(1);
        self.services.limits.root_fix_rounds = self.services.limits.root_fix_rounds.max(1);
        self.services.limits.max_total_tasks = self.services.limits.max_total_tasks.max(1);

        self.state.set_root_id(root_id);
        // Register all tasks for TUI (root + any pre-existing subtasks on resume).
        for id in self.state.dfs_order(root_id) {
            if let Some(t) = self.state.get(id) {
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
        self.execute_task(root_id).await
    }

    pub fn into_state(self) -> EpicState {
        self.state
    }

    /// Write state to disk if a state path is configured. Best-effort: logs
    /// but does not propagate write errors to avoid aborting the run.
    fn checkpoint_save(&self) {
        if let Some(ref path) = self.services.state_path {
            if let Err(e) = self.state.save(path) {
                eprintln!("warning: state checkpoint failed: {e}");
            }
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.services.events.send(event);
    }

    fn transition(&mut self, id: TaskId, phase: TaskPhase) -> Result<(), OrchestratorError> {
        let task = self
            .state
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        task.phase = phase;
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

    async fn try_verify(&mut self, id: TaskId) -> Result<VerifyOutcome, OrchestratorError> {
        let verify_model = self.verification_model(id)?;
        let ctx = self.build_context(id)?;
        match self.services.agent.verify(&ctx, verify_model).await {
            Ok(agent_result) => {
                self.accumulate_usage(id, &agent_result.meta);
                match agent_result.value.outcome {
                    VerificationOutcome::Pass => {
                        if let Some(fail_reason) = self.try_file_level_review(id).await? {
                            Ok(VerifyOutcome::Failed(fail_reason))
                        } else {
                            self.complete_task_verified(id)?;
                            Ok(VerifyOutcome::Passed)
                        }
                    }
                    VerificationOutcome::Fail { reason } => {
                        self.record_to_vault(id, "VERIFICATION_FAILURE", &reason)
                            .await;
                        self.checkpoint_save();
                        Ok(VerifyOutcome::Failed(reason))
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: verify failed: {e}");
                self.checkpoint_save();
                Ok(VerifyOutcome::Failed(format!("verification error: {e}")))
            }
        }
    }

    /// Run file-level review for leaf tasks after verification passes.
    /// Returns `Some(reason)` on failure, `None` on pass or skip (non-leaf).
    async fn try_file_level_review(
        &mut self,
        id: TaskId,
    ) -> Result<Option<String>, OrchestratorError> {
        let task = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        if task.path != Some(TaskPath::Leaf) {
            return Ok(None);
        }

        let review_model = self.verification_model(id)?;
        let review_ctx = self.build_context(id)?;
        let review_result = self
            .services
            .agent
            .file_level_review(&review_ctx, review_model)
            .await?;
        self.accumulate_usage(id, &review_result.meta);

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
    fn check_task_limit(&self, parent_id: TaskId, count: usize) -> Option<String> {
        let current = self.state.task_count();
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
        &mut self,
        parent_id: TaskId,
        specs: Vec<SubtaskSpec>,
        mark_fix: bool,
        append: bool,
        inherit_recovery_rounds: Option<u32>,
    ) -> Result<Vec<TaskId>, OrchestratorError> {
        let parent_depth = self
            .state
            .get(parent_id)
            .ok_or(OrchestratorError::TaskNotFound(parent_id))?
            .depth;

        let mut child_ids = Vec::new();
        for spec in specs {
            let child_id = self.state.next_task_id();
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
            self.state.insert(child);
        }

        {
            let task = self
                .state
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            if append {
                task.subtask_ids.extend_from_slice(&child_ids);
            } else {
                task.subtask_ids.clone_from(&child_ids);
            }
        }

        for &child_id in &child_ids {
            if let Some(child) = self.state.get(child_id) {
                self.emit(Event::TaskRegistered {
                    task_id: child_id,
                    parent_id: child.parent_id,
                    goal: child.goal.clone(),
                    depth: child.depth,
                });
            }
        }
        self.checkpoint_save();

        Ok(child_ids)
    }

    fn build_context(&self, id: TaskId) -> Result<TaskContext, OrchestratorError> {
        context::build_context(&self.state, id)
    }

    // Returns boxed future to support recursion (execute_task → execute_branch → execute_task).
    fn execute_task(
        &mut self,
        id: TaskId,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutcome, OrchestratorError>> + Send + '_>> {
        Box::pin(async move {
            // Resume: skip already-terminal tasks.
            {
                let task = self
                    .state
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
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
                let task = self
                    .state
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                let path = task.path.clone();
                let phase = task.phase;

                if let (Some(p), TaskPhase::Verifying) = (&path, phase) {
                    return match p {
                        TaskPath::Leaf => self.run_leaf(id).await,
                        TaskPath::Branch => self.finalize_branch(id, TaskOutcome::Success).await,
                    };
                }

                if let (Some(p), TaskPhase::Executing) = (&path, phase) {
                    let p = p.clone();
                    self.transition(id, TaskPhase::Executing)?;
                    return match p {
                        TaskPath::Leaf => self.run_leaf(id).await,
                        TaskPath::Branch => {
                            let outcome = self.execute_branch(id).await?;
                            self.finalize_branch(id, outcome).await
                        }
                    };
                }
            }

            self.transition(id, TaskPhase::Assessing)?;

            let task = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
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
                let ctx = self.build_context(id)?;
                let agent_result = self.services.agent.assess(&ctx).await?;
                self.accumulate_usage(id, &agent_result.meta);
                agent_result.value
            };

            // Apply assessment to task.
            {
                let task = self
                    .state
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

    fn verification_model(&self, id: TaskId) -> Result<Model, OrchestratorError> {
        let task = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        Ok(task.verification_model())
    }

    /// Branch-only finalization: verify + fix loop. Leaf tasks use `Task::execute_leaf`.
    async fn finalize_branch(
        &mut self,
        id: TaskId,
        outcome: TaskOutcome,
    ) -> Result<TaskOutcome, OrchestratorError> {
        if outcome == TaskOutcome::Success {
            self.transition(id, TaskPhase::Verifying)?;

            let tree = context::build_tree_context(&self.state, id)?;
            let task = self
                .state
                .get_mut(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            let verify_outcome = task.verify_branch(&tree, &self.services).await?;
            let is_fix_task = task.is_fix_task;

            match verify_outcome {
                crate::task::branch::BranchVerifyOutcome::Passed => self.complete_task_verified(id),
                crate::task::branch::BranchVerifyOutcome::Failed { reason } => {
                    if is_fix_task {
                        self.fail_task(id, reason)
                    } else {
                        self.branch_fix_loop(id, &reason).await
                    }
                }
            }
        } else {
            // outcome is already Failed; extract reason for fail_task helper.
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
        let is_root = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .parent_id
            .is_none();

        let mut failure_reason = initial_failure.to_owned();

        loop {
            // Check round budget via Task method.
            let model = {
                let task = self
                    .state
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.fix_round_budget_check(is_root, &self.services.limits) {
                    crate::task::branch::FixBudgetCheck::Exhausted => {
                        return self.fail_task(id, failure_reason);
                    }
                    crate::task::branch::FixBudgetCheck::WithinBudget { model } => model,
                }
            };

            let round = {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.increment_fix_rounds()
            };

            // Scope circuit breaker via Task method.
            {
                let task = self
                    .state
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                match task.check_branch_scope(&self.services).await {
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

            // Design fix subtasks via Task method.
            let tree = context::build_tree_context(&self.state, id)?;
            let specs = {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.design_fix(&tree, &self.services, &failure_reason, round, model)
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

            // Cross-task: create subtasks (stays in orchestrator).
            let fix_child_ids = self.create_subtasks(id, subtask_specs, true, true, None)?;
            self.emit(Event::FixSubtasksCreated {
                task_id: id,
                count: fix_child_ids.len(),
                round,
            });

            // Cross-task: execute each fix subtask (stays in orchestrator).
            for &child_id in &fix_child_ids {
                let _child_outcome = self.execute_task(child_id).await?;
            }

            // Re-verify the branch after fix subtasks complete.
            match self.try_verify(id).await? {
                VerifyOutcome::Passed => return Ok(TaskOutcome::Success),
                VerifyOutcome::Failed(reason) => failure_reason = reason,
            }
        }
    }

    async fn run_leaf(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        let tree = context::build_tree_context(&self.state, id)?;
        let task = self
            .state
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        let outcome = task.execute_leaf(&tree, &self.services).await;
        // Agent-level errors propagated as infrastructure failures.
        if let TaskOutcome::Failed { ref reason } = outcome {
            if let Some(msg) = reason.strip_prefix("__agent_error__: ") {
                return Err(OrchestratorError::Agent(anyhow::anyhow!("{msg}")));
            }
        }
        self.complete_or_fail(id, outcome)
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_branch(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        // Resume: reuse existing subtasks if already decomposed.
        let existing_subtasks = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .subtask_ids
            .clone();

        if existing_subtasks.is_empty() {
            let task = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            let decompose_model = task.current_model.unwrap_or(Model::Sonnet);
            let ctx = self.build_context(id)?;
            let agent_result = self
                .services
                .agent
                .design_and_decompose(&ctx, decompose_model)
                .await?;
            self.accumulate_usage(id, &agent_result.meta);
            let decomposition = agent_result.value;

            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.set_decomposition_rationale(decomposition.rationale);
            }

            if let Some(reason) = self.check_task_limit(id, decomposition.subtasks.len()) {
                return Ok(TaskOutcome::Failed { reason });
            }

            let new_child_ids =
                self.create_subtasks(id, decomposition.subtasks, false, false, None)?;
            self.emit(Event::SubtasksCreated {
                parent_id: id,
                child_ids: new_child_ids,
            });
        }

        // Outer loop: restarts child iteration after recovery creates new subtasks.
        loop {
            let child_ids = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .subtask_ids
                .clone();

            let mut all_done = true;

            for &child_id in &child_ids {
                let child_phase = self
                    .state
                    .get(child_id)
                    .ok_or(OrchestratorError::TaskNotFound(child_id))?
                    .phase;

                match child_phase {
                    TaskPhase::Completed | TaskPhase::Failed => continue,
                    _ => {}
                }

                all_done = false;
                let child_outcome = self.execute_task(child_id).await?;

                // Check for discoveries → checkpoint.
                let child_discoveries = self
                    .state
                    .get(child_id)
                    .ok_or(OrchestratorError::TaskNotFound(child_id))?
                    .discoveries
                    .clone();

                if !child_discoveries.is_empty() {
                    use crate::task::branch::ChildResponse;

                    let tree = context::build_tree_context(&self.state, id)?;
                    let response = {
                        let task = self
                            .state
                            .get_mut(id)
                            .ok_or(OrchestratorError::TaskNotFound(id))?;
                        task.handle_checkpoint(&tree, &self.services, &child_discoveries)
                            .await?
                    };
                    self.checkpoint_save();

                    match response {
                        ChildResponse::Continue => {}
                        ChildResponse::NeedRecoverySubtasks {
                            specs,
                            supersede_pending,
                        } => {
                            // Cross-task: mark pending children as Failed.
                            if supersede_pending {
                                let existing_child_ids = self
                                    .state
                                    .get(id)
                                    .ok_or(OrchestratorError::TaskNotFound(id))?
                                    .subtask_ids
                                    .clone();
                                for &eid in &existing_child_ids {
                                    let existing_child = self
                                        .state
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
                            if let Some(msg) = self.check_task_limit(id, specs.len()) {
                                return Ok(TaskOutcome::Failed {
                                    reason: format!("{msg}: checkpoint escalation"),
                                });
                            }
                            let count = specs.len();
                            let parent_rounds = self
                                .state
                                .get(id)
                                .ok_or(OrchestratorError::TaskNotFound(id))?
                                .recovery_rounds;
                            self.create_subtasks(id, specs, false, true, Some(parent_rounds))?;
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
                    if let Some(recovery_outcome) = self.attempt_recovery(id, reason).await? {
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
        self.reorganize_vault(id).await;

        // Guard: if every non-fix child failed (recovery exhausted or skipped),
        // the branch itself must report failure rather than vacuous success.
        let child_ids = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .subtask_ids
            .clone();
        let any_non_fix_succeeded = child_ids.iter().any(|&cid| {
            self.state
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
        &mut self,
        parent_id: TaskId,
        failure_reason: &str,
    ) -> Result<Option<TaskOutcome>, OrchestratorError> {
        use crate::task::branch::RecoveryDecision;

        let task = self
            .state
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
        let tree = context::build_tree_context(&self.state, parent_id)?;
        let decision = {
            let task = self
                .state
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            task.assess_and_design_recovery(&tree, &self.services, failure_reason, round)
                .await?
        };
        self.checkpoint_save();

        match decision {
            RecoveryDecision::Unrecoverable { reason } => Ok(Some(TaskOutcome::Failed { reason })),
            RecoveryDecision::Plan {
                specs,
                supersede_pending,
            } => {
                // Cross-task: mark pending children as Failed for full re-decomposition.
                if supersede_pending {
                    let child_ids = self
                        .state
                        .get(parent_id)
                        .ok_or(OrchestratorError::TaskNotFound(parent_id))?
                        .subtask_ids
                        .clone();

                    for &child_id in &child_ids {
                        let child = self
                            .state
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
                if let Some(msg) = self.check_task_limit(parent_id, specs.len()) {
                    return Ok(Some(TaskOutcome::Failed {
                        reason: format!("{msg}: {failure_reason}"),
                    }));
                }

                let count = specs.len();

                // Read parent's recovery round counter before creating subtasks so that
                // children inherit it during creation (prevents exponential cost growth).
                let parent_rounds = self
                    .state
                    .get(parent_id)
                    .ok_or(OrchestratorError::TaskNotFound(parent_id))?
                    .recovery_rounds;
                self.create_subtasks(parent_id, specs, false, true, Some(parent_rounds))?;

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
mod tests {
    use super::*;
    use crate::agent::ChildStatus;
    use crate::events::{self, EventReceiver};
    use crate::task::branch::{CheckpointDecision, DecompositionResult, SubtaskSpec};
    use crate::task::verify::{VerificationOutcome, VerificationResult};
    use crate::task::{Attempt, LeafResult, Magnitude, MagnitudeEstimate, RecoveryPlan, TaskPath};
    use crate::test_support::MockAgentService;

    fn pass_verification() -> VerificationResult {
        VerificationResult {
            outcome: VerificationOutcome::Pass,
            details: "all checks passed".into(),
        }
    }

    fn pass_file_level_review() -> VerificationResult {
        VerificationResult {
            outcome: VerificationOutcome::Pass,
            details: "file-level review passed".into(),
        }
    }

    fn fail_file_level_review(reason: &str) -> VerificationResult {
        VerificationResult {
            outcome: VerificationOutcome::Fail {
                reason: reason.into(),
            },
            details: "file-level review failed".into(),
        }
    }

    fn queue_file_level_reviews(mock: &MockAgentService, count: usize) {
        for _ in 0..count {
            mock.file_level_review_responses
                .lock()
                .unwrap()
                .push_back(pass_file_level_review());
        }
    }

    fn one_subtask_decomposition() -> DecompositionResult {
        DecompositionResult {
            subtasks: vec![SubtaskSpec {
                goal: "child task".into(),
                verification_criteria: vec!["child passes".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            }],
            rationale: "single subtask".into(),
        }
    }

    fn leaf_assessment() -> AssessmentResult {
        AssessmentResult {
            path: TaskPath::Leaf,
            model: Model::Haiku,
            rationale: "simple task".into(),
            magnitude: None,
        }
    }

    fn leaf_success() -> LeafResult {
        LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: Vec::new(),
        }
    }

    fn leaf_failed(reason: &str) -> LeafResult {
        LeafResult {
            outcome: TaskOutcome::Failed {
                reason: reason.into(),
            },
            discoveries: Vec::new(),
        }
    }

    // Root gets TaskId(0); subtasks get sequential IDs (TaskId(1), TaskId(2), ...) in creation order.
    fn make_orchestrator(
        mock: MockAgentService,
    ) -> (Orchestrator<MockAgentService>, TaskId, EventReceiver) {
        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let root = Task::new(
            root_id,
            None,
            "root goal".into(),
            vec!["root passes".into()],
            0,
        );
        state.insert(root);
        let (tx, rx) = events::event_channel();
        let orchestrator = Orchestrator::new(mock, state, tx);
        (orchestrator, root_id, rx)
    }

    /// Root(branch) → one child(leaf) → success → verification pass → Completed.
    #[tokio::test]
    async fn single_leaf() {
        let mock = MockAgentService::new();

        // Root branches (forced), decomposition returns 1 subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child leaf execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.phase, TaskPhase::Completed);
        assert_eq!(root.path, Some(TaskPath::Branch));

        let child_id = root.subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        assert_eq!(child.path, Some(TaskPath::Leaf));
    }

    /// Root decomposes into 2 → both succeed → root Completed.
    #[tokio::test]
    async fn two_children() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Medium,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Both assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Both succeed.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child A, child B, root — all pass.
        for _ in 0..3 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(orch.state.get(root_id).unwrap().subtask_ids.len(), 2);
    }

    /// Haiku fails 3x → escalate to Sonnet → succeeds.
    #[tokio::test]
    async fn leaf_retry_and_escalation() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // 3 Haiku failures, then 1 Sonnet success.
        for _ in 0..3 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("haiku failed"));
        }
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.attempts.len(), 4);
        assert_eq!(child.current_model, Some(Model::Sonnet));
    }

    /// All tiers exhausted → leaf Failed → parent Failed.
    #[tokio::test]
    async fn terminal_failure() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // 3 Haiku + 3 Sonnet + 3 Opus failures = 9 total.
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("persistent failure"));
        }

        // Recovery assessment called once (budget=2, but branch fails immediately).
        mock.recovery_responses.lock().unwrap().push_back(None);

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.attempts.len(), 9);
        assert_eq!(child.phase, TaskPhase::Failed);
        assert_eq!(orch.state.get(root_id).unwrap().phase, TaskPhase::Failed);
    }

    /// State is checkpointed to disk during execution.
    #[tokio::test]
    async fn checkpoint_saves_state() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let dir = std::env::temp_dir().join("epic_test_checkpoint");
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("state.json");

        // Clean up any previous run.
        let _ = std::fs::remove_file(&state_path);

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.state.set_root_id(root_id);
        orch.services.state_path = Some(state_path.clone());

        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // State file should exist from checkpoint writes.
        assert!(state_path.exists(), "state.json should exist after run");

        // Load and verify it contains the completed task tree.
        let loaded = EpicState::load(&state_path).unwrap();
        assert_eq!(loaded.root_id(), Some(root_id));
        let loaded_root = loaded.get(root_id).unwrap();
        assert_eq!(loaded_root.phase, TaskPhase::Completed);
        assert!(!loaded_root.subtask_ids.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Resume: completed child is NOT re-executed; pending child runs normally.
    #[tokio::test]
    async fn resume_skips_completed_child() {
        let mock = MockAgentService::new();

        // No decompose response — root already has subtask_ids.
        // No assess/leaf response for completed child — would panic if called.

        // Pending child: assessed as leaf, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: pending child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let completed_child_id = state.next_task_id();
        let pending_child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![completed_child_id, pending_child_id];

        let mut completed_child = Task::new(
            completed_child_id,
            Some(root_id),
            "done".into(),
            vec!["done".into()],
            1,
        );
        completed_child.phase = TaskPhase::Completed;

        let pending_child = Task::new(
            pending_child_id,
            Some(root_id),
            "todo".into(),
            vec!["todo".into()],
            1,
        );

        state.insert(root);
        state.insert(completed_child);
        state.insert(pending_child);

        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        assert_eq!(
            orch.state.get(completed_child_id).unwrap().phase,
            TaskPhase::Completed
        );
        assert_eq!(
            orch.state.get(pending_child_id).unwrap().phase,
            TaskPhase::Completed
        );
    }

    /// Resume: existing `subtask_ids` on root skips decomposition.
    #[tokio::test]
    async fn resume_skips_decomposition_when_subtasks_exist() {
        let mock = MockAgentService::new();

        // No decompose response queued — would panic if called.

        // Child: assessed as leaf, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
        root.subtask_ids = vec![child_id];

        let child = Task::new(
            child_id,
            Some(root_id),
            "existing child".into(),
            vec!["child passes".into()],
            1,
        );

        state.insert(root);
        state.insert(child);

        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.subtask_ids, vec![child_id]);
        assert_eq!(root.phase, TaskPhase::Completed);
    }

    /// Resume: mid-execution Branch is NOT re-assessed; uses existing path and children.
    #[tokio::test]
    async fn resume_mid_execution_branch_not_reassessed() {
        let mock = MockAgentService::new();

        // No assess or decompose responses queued — would panic if called.

        // Grandchild: assessed as leaf, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: grandchild, middle branch, root — all pass.
        for _ in 0..3 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        let mut state = EpicState::new();
        let root_id = state.next_task_id(); // T0
        let mid_id = state.next_task_id(); // T1
        let grandchild_id = state.next_task_id(); // T2

        // Root: Executing, Branch, has mid as child.
        let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![mid_id];

        // Mid: Executing, Branch, has grandchild. Was mid-execution when killed.
        let mut mid = Task::new(
            mid_id,
            Some(root_id),
            "mid".into(),
            vec!["mid passes".into()],
            1,
        );
        mid.path = Some(TaskPath::Branch);
        mid.current_model = Some(Model::Sonnet);
        mid.phase = TaskPhase::Executing;
        mid.subtask_ids = vec![grandchild_id];

        // Grandchild: Pending, not yet executed.
        let grandchild = Task::new(
            grandchild_id,
            Some(mid_id),
            "grandchild".into(),
            vec!["gc passes".into()],
            2,
        );

        state.insert(root);
        state.insert(mid);
        state.insert(grandchild);

        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 3);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Mid was NOT re-assessed — still Branch with same child.
        let mid = orch.state.get(mid_id).unwrap();
        assert_eq!(mid.path, Some(TaskPath::Branch));
        assert_eq!(mid.subtask_ids, vec![grandchild_id]);
        assert_eq!(mid.phase, TaskPhase::Completed);
    }

    /// Resume: task in Verifying phase goes straight to re-verification, not re-execution.
    #[tokio::test]
    async fn resume_verifying_skips_execution() {
        let mock = MockAgentService::new();

        // No decompose, assess, or leaf responses — would panic if re-executed.

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![child_id];

        // Child was mid-verification when killed.
        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child".into(),
            vec!["child passes".into()],
            1,
        );
        child.path = Some(TaskPath::Leaf);
        child.current_model = Some(Model::Haiku);
        child.phase = TaskPhase::Verifying;

        state.insert(root);
        state.insert(child);

        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Child went straight to verification, not re-execution.
        assert_eq!(
            orch.state.get(child_id).unwrap().phase,
            TaskPhase::Completed
        );
        // No attempts added — leaf was not re-executed.
        assert!(orch.state.get(child_id).unwrap().attempts.is_empty());
    }

    /// Task at max depth forced to Leaf path.
    #[tokio::test]
    async fn depth_cap_forces_leaf() {
        let mock = MockAgentService::new();

        // Root branches, decomposition returns 1 subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child leaf execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Set up state with root at depth max_depth - 1 so child hits cap.
        let limits = LimitsConfig::default();
        let max_depth = limits.max_depth;
        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let root = Task::new(
            root_id,
            None,
            "deep root".into(),
            vec!["passes".into()],
            max_depth - 1,
        );
        state.insert(root);
        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);

        // Root is not at depth 0 but has no parent, so it's forced to Branch.
        // Child will be at max_depth, forced to Leaf (no assess call needed).
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.path, Some(TaskPath::Leaf));
        assert_eq!(child.depth, max_depth);
    }

    /// Leaf reports discoveries → stored on task → checkpoint called → sibling sees them.
    #[tokio::test]
    async fn discoveries_propagated_to_checkpoint() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Both assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["API uses v2 format".into(), "cache layer found".into()],
        });

        // Checkpoint after child A's discoveries.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Proceed);

        // Child B succeeds (no discoveries).
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child A, child B, root — all pass.
        for _ in 0..3 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Child A should have discoveries stored.
        let child_a_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child_a = orch.state.get(child_a_id).unwrap();
        assert_eq!(
            child_a.discoveries,
            vec!["API uses v2 format", "cache layer found"]
        );

        // DiscoveriesRecorded event should have been emitted.
        let mut found_discoveries_event = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::DiscoveriesRecorded { task_id, count } if task_id == child_a_id && count == 2)
            {
                found_discoveries_event = true;
            }
        }
        assert!(
            found_discoveries_event,
            "DiscoveriesRecorded event not found"
        );
    }

    /// Task with magnitude set but no git repo → `WithinBounds` (best-effort).
    #[tokio::test]
    async fn scope_check_within_bounds() {
        let mock = MockAgentService::new();
        let mut state = EpicState::new();
        let task_id = state.next_task_id();
        let mut task = Task::new(task_id, None, "test".into(), vec![], 0);
        task.magnitude = Some(Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 3,
        });
        state.insert(task);

        let (tx, _rx) = events::event_channel();
        let orch = Orchestrator::new(mock, state, tx)
            .with_project_root(PathBuf::from("/nonexistent/path"));

        // git will fail on a nonexistent path → best-effort WithinBounds.
        let task = orch.state.get(task_id).unwrap();
        let result = task.check_branch_scope(&orch.services).await;
        assert!(matches!(result, ScopeCheck::WithinBounds));
    }

    /// Task with no magnitude → `WithinBounds` (skip check).
    #[tokio::test]
    async fn scope_check_skipped_no_magnitude() {
        let mock = MockAgentService::new();
        let mut state = EpicState::new();
        let task_id = state.next_task_id();
        let task = Task::new(task_id, None, "test".into(), vec![], 0);
        state.insert(task);

        let (tx, _rx) = events::event_channel();
        let orch = Orchestrator::new(mock, state, tx);

        let task = orch.state.get(task_id).unwrap();
        let result = task.check_branch_scope(&orch.services).await;
        assert!(matches!(result, ScopeCheck::WithinBounds));
    }

    fn fail_verification(reason: &str) -> VerificationResult {
        VerificationResult {
            outcome: VerificationOutcome::Fail {
                reason: reason.into(),
            },
            details: "check failed".into(),
        }
    }

    /// Leaf fix loop: verification fails → `fix_leaf` succeeds → re-verification passes.
    #[tokio::test]
    async fn leaf_fix_passes_on_retry() {
        let mock = MockAgentService::new();

        // Root branches, 1 subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf/haiku.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child leaf execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child fails first time.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("test X not passing"));

        // Fix attempt succeeds.
        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Re-verification after fix: passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        assert_eq!(child.fix_attempts.len(), 1);
        assert!(child.fix_attempts[0].succeeded);
    }

    /// Leaf fix loop: 3 failures at starting tier → escalate → fix succeeds → verify passes.
    #[tokio::test]
    async fn leaf_fix_escalates_model() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Leaf execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Initial verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("tests fail"));

        // 3 fix failures at Haiku tier.
        for _ in 0..3 {
            mock.fix_leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("could not fix"));
        }

        // After escalation to Sonnet, fix succeeds.
        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.fix_attempts.len(), 4);
        assert_eq!(child.current_model, Some(Model::Sonnet));

        // Check FixModelEscalated event was emitted.
        let mut found_escalation = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::FixModelEscalated { task_id, from: Model::Haiku, to: Model::Sonnet } if task_id == child_id)
            {
                found_escalation = true;
            }
        }
        assert!(found_escalation, "FixModelEscalated event not found");
    }

    /// Leaf fix loop: all tiers exhausted (9 fix failures) → terminal failure.
    #[tokio::test]
    async fn leaf_fix_terminal_failure() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Leaf execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Initial verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("tests fail"));

        // 9 fix failures: 3 haiku + 3 sonnet + 3 opus.
        for _ in 0..9 {
            mock.fix_leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("still broken"));
        }

        // Recovery assessment for branch failure.
        mock.recovery_responses.lock().unwrap().push_back(None);

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.fix_attempts.len(), 9);
        assert_eq!(child.phase, TaskPhase::Failed);
    }

    fn one_fix_subtask_decomposition() -> DecompositionResult {
        DecompositionResult {
            subtasks: vec![SubtaskSpec {
                goal: "fix subtask".into(),
                verification_criteria: vec!["fix passes".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            }],
            rationale: "targeted fix".into(),
        }
    }

    /// Push mock responses for: root decomposes to 1 leaf child that succeeds
    /// and passes verification, then root verification fails with `root_fail_reason`.
    fn setup_branch_with_failing_root_verify(mock: &MockAgentService, root_fail_reason: &str) {
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification(root_fail_reason));
    }

    /// Branch fix loop: root verification fails → fix subtask created → re-verify passes.
    #[tokio::test]
    async fn branch_fix_creates_subtasks() {
        let mock = MockAgentService::new();

        // Root branches, 1 subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Original child: assessed as leaf/haiku, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root check failed"));

        // Branch fix loop: design_fix_subtasks returns 1 fix subtask.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        // Fix subtask: assessed as leaf/haiku, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Fix subtask verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.subtask_ids.len(), 2); // original + fix
        assert_eq!(root.verification_fix_rounds, 1);
        assert_eq!(root.phase, TaskPhase::Completed);

        let fix_id = root.subtask_ids[1];
        let fix_task = orch.state.get(fix_id).unwrap();
        assert!(fix_task.is_fix_task);
        assert_eq!(fix_task.phase, TaskPhase::Completed);
    }

    /// Branch fix loop: non-root branch exhausts 3 rounds → terminal failure.
    #[tokio::test]
    async fn branch_fix_round_budget() {
        let mock = MockAgentService::new();

        // Root branches into mid (a branch).
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "mid branch".into(),
                    verification_criteria: vec!["mid passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Medium,
                }],
                rationale: "one mid branch".into(),
            });

        // Mid is assessed as branch.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            });

        // Mid decomposes into 1 leaf child.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Mid's child: assessed as leaf, executes, succeeds, verification passes.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Mid verification fails initially.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("mid check failed"));

        // 3 rounds of fix subtasks, each round:
        // - design_fix_subtasks returns 1 fix subtask
        // - fix subtask assessed as leaf, succeeds, verification passes
        // - mid re-verification fails
        for _ in 0..3 {
            mock.fix_subtask_responses
                .lock()
                .unwrap()
                .push_back(one_fix_subtask_decomposition());

            mock.assess_responses
                .lock()
                .unwrap()
                .push_back(leaf_assessment());
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_success());
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());

            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(fail_verification("still failing"));
        }

        // Mid fails → recovery assessment for root.
        mock.recovery_responses.lock().unwrap().push_back(None);

        queue_file_level_reviews(&mock, 4);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        // Find mid task.
        let mid_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let mid = orch.state.get(mid_id).unwrap();
        assert_eq!(mid.verification_fix_rounds, 3);
        assert_eq!(mid.phase, TaskPhase::Failed);
        // Original child + 3 fix subtasks = 4 subtasks total.
        assert_eq!(mid.subtask_ids.len(), 4);
    }

    /// Fix subtask that is itself a branch does NOT trigger branch fix loop on verification failure.
    #[tokio::test]
    async fn branch_fix_subtasks_no_recursive_fix() {
        let mock = MockAgentService::new();

        // Root branches, 1 subtask (original child).
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Original child: leaf, succeeds, passes verification.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root check failed"));

        // Branch fix round 1: design_fix_subtasks returns 1 fix subtask that will be assessed as branch.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "complex fix".into(),
                    verification_criteria: vec!["fix passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Medium,
                }],
                rationale: "complex fix needed".into(),
            });

        // Fix subtask assessed as branch.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            });

        // Fix subtask decomposes into 1 grandchild leaf.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Grandchild: leaf, succeeds, passes verification.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Fix subtask (branch) verification FAILS — should NOT trigger branch fix loop
        // because is_fix_task == true. Should fail immediately.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("fix branch failed"));

        // Root re-verification after round 1 (fix subtask failed, but re-verify anyway).
        // Actually, the fix subtask failure propagates: execute_task returns Failed for the
        // fix subtask, but the branch_fix_loop still re-verifies the root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root still failing"));

        // Round 2: simple fix subtask that succeeds.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 4);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.verification_fix_rounds, 2);

        // The fix subtask from round 1 should be marked as fix and failed.
        let fix1_id = root.subtask_ids[1];
        let fix1 = orch.state.get(fix1_id).unwrap();
        assert!(fix1.is_fix_task);
        assert_eq!(fix1.phase, TaskPhase::Failed);
    }

    /// Leaf fix subtask that fails verification does NOT enter leaf fix loop.
    /// Fix tasks (leaf or branch) fail immediately to prevent recursive fix-within-fix.
    #[tokio::test]
    async fn leaf_fix_subtask_no_recursive_fix_loop() {
        let mock = MockAgentService::new();

        // Root branches into 1 subtask (original child).
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Branch fix round 1: design_fix_subtasks returns 1 fix subtask (assessed as leaf).
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        // Fix subtask assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Fix subtask executes successfully.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Fix subtask verification FAILS — must NOT enter leaf fix loop since is_fix_task.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("fix leaf failed"));

        // Root re-verification after round 1 (fix subtask failed, re-verify anyway).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root still failing"));

        // Round 2: simple fix subtask that succeeds and passes verification.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.verification_fix_rounds, 2);

        // The leaf fix subtask from round 1 should be marked as fix and failed.
        let fix1_id = root.subtask_ids[1];
        let fix1 = orch.state.get(fix1_id).unwrap();
        assert!(fix1.is_fix_task);
        assert_eq!(fix1.path, Some(TaskPath::Leaf));
        assert_eq!(fix1.phase, TaskPhase::Failed);
        // Must have zero fix attempts — it should NOT have entered the leaf fix loop.
        assert_eq!(fix1.fix_attempts.len(), 0);
    }

    /// Branch fix subtask that fails verification is failed immediately (no recursive `branch_fix_loop`).
    #[tokio::test]
    async fn branch_fix_subtask_no_recursive_fix_loop() {
        let mock = MockAgentService::new();

        // Root branches into 1 child (original child succeeds, root verification fails).
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Branch fix round 1: design_fix_subtasks returns 1 fix subtask.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        // Fix subtask assessed as BRANCH (not leaf).
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            });

        // Fix subtask decomposes into 1 grandchild.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Grandchild assessed as leaf, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Grandchild verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Branch fix subtask verification FAILS — must fail immediately (is_fix_task guard).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("branch fix subtask failed"));

        // Root re-verification after round 1 (fix subtask failed).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root still failing"));

        // Round 2: simple fix subtask that succeeds and passes verification.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.verification_fix_rounds, 2);

        // The branch fix subtask from round 1 should be marked as fix and failed.
        let fix1_id = root.subtask_ids[1];
        let fix1 = orch.state.get(fix1_id).unwrap();
        assert!(fix1.is_fix_task);
        assert_eq!(fix1.path, Some(TaskPath::Branch));
        assert_eq!(fix1.phase, TaskPhase::Failed);
        // Must have zero verification_fix_rounds — should NOT have entered branch_fix_loop.
        assert_eq!(
            fix1.verification_fix_rounds, 0,
            "branch fix subtask should not enter its own branch_fix_loop"
        );
    }

    /// Resume mid-fix-loop: pre-existing `fix_attempts` are counted so `retries_at_tier` is correct.
    #[tokio::test]
    async fn leaf_fix_persists_and_resumes() {
        let mock = MockAgentService::new();

        // The child is already in Verifying with 2 fix_attempts at Haiku.
        // execute_task sees Verifying → finalize_branch(Success) → verify → fail → leaf_retry_loop(Fix).
        // leaf_retry_loop initializes retries_at_tier=2 from the 2 existing fix_attempts.
        // Loop: scope check (WithinBounds, no magnitude) → fix(success) → record(#3) → verify(pass).

        // Mock sequence: verify(child fail) → fix_leaf(success) → verify(child pass) → verify(root pass).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("still broken"));

        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child".into(),
            vec!["child passes".into()],
            1,
        );
        child.path = Some(TaskPath::Leaf);
        child.current_model = Some(Model::Haiku);
        child.phase = TaskPhase::Verifying;
        child.fix_attempts = vec![
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("fail1".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("fail2".into()),
            },
        ];

        state.insert(root);
        state.insert(child);

        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        assert_eq!(child.fix_attempts.len(), 3); // 2 pre-existing + 1 new
        assert!(child.fix_attempts[2].succeeded);
        assert_eq!(child.fix_attempts[2].model, Model::Haiku);
    }

    /// Resume fix loop with exhausted tier: escalates immediately without extra attempt.
    #[tokio::test]
    async fn leaf_fix_resume_escalates_immediately_when_tier_exhausted() {
        let mock = MockAgentService::new();

        // Child is Verifying with 3 failed fix attempts at Haiku (tier exhausted).
        // Crash happened before escalation. On resume: should escalate to Sonnet
        // immediately without executing a 4th Haiku fix attempt.

        // Mock sequence: verify(child fail) → fix_leaf at Sonnet(success) → verify(child pass) → verify(root pass).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("still broken"));

        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child".into(),
            vec!["child passes".into()],
            1,
        );
        child.path = Some(TaskPath::Leaf);
        child.current_model = Some(Model::Haiku); // Not yet escalated.
        child.phase = TaskPhase::Verifying;
        child.fix_attempts = vec![
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("f1".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("f2".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("f3".into()),
            },
        ];

        state.insert(root);
        state.insert(child);

        let (tx, mut rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        // 3 pre-existing Haiku + 1 successful Sonnet fix = 4 (no extra Haiku attempt).
        assert_eq!(child.fix_attempts.len(), 4);
        assert_eq!(child.fix_attempts[3].model, Model::Sonnet);
        assert!(child.fix_attempts[3].succeeded);

        // Verify escalation event.
        let mut saw_escalation = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event,
                Event::FixModelEscalated {
                    from: Model::Haiku,
                    to: Model::Sonnet,
                    ..
                }
            ) {
                saw_escalation = true;
            }
        }
        assert!(
            saw_escalation,
            "FixModelEscalated Haiku→Sonnet expected on immediate escalation"
        );
    }

    /// Branch fix loop: root gets 4th round at Opus after 3 Sonnet rounds fail.
    #[tokio::test]
    async fn branch_fix_root_opus_round() {
        let mock = MockAgentService::new();

        // Root branches, 1 original subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Original child: leaf, succeeds, verification passes.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root check failed"));

        // 4 rounds of fix subtasks.
        // Rounds 1-3 (Sonnet): fix subtask succeeds, root re-verify fails.
        // Round 4 (Opus): fix subtask succeeds, root re-verify passes.
        for round in 1..=4 {
            mock.fix_subtask_responses
                .lock()
                .unwrap()
                .push_back(one_fix_subtask_decomposition());

            mock.assess_responses
                .lock()
                .unwrap()
                .push_back(leaf_assessment());
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_success());
            // Fix subtask verification passes.
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());

            if round < 4 {
                // Root re-verification fails.
                mock.verify_responses
                    .lock()
                    .unwrap()
                    .push_back(fail_verification("root still failing"));
            } else {
                // Root re-verification passes on round 4.
                mock.verify_responses
                    .lock()
                    .unwrap()
                    .push_back(pass_verification());
            }
        }

        queue_file_level_reviews(&mock, 5);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.verification_fix_rounds, 4);
        assert_eq!(root.phase, TaskPhase::Completed);
        // 1 original + 4 fix subtasks = 5 total.
        assert_eq!(root.subtask_ids.len(), 5);

        // Check BranchFixRound events: rounds 1-3 at Sonnet, round 4 at Opus.
        let mut branch_fix_rounds: Vec<(u32, Model)> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let Event::BranchFixRound {
                task_id,
                round,
                model,
            } = event
            {
                if task_id == root_id {
                    branch_fix_rounds.push((round, model));
                }
            }
        }
        assert_eq!(branch_fix_rounds.len(), 4);
        assert_eq!(branch_fix_rounds[0], (1, Model::Sonnet));
        assert_eq!(branch_fix_rounds[1], (2, Model::Sonnet));
        assert_eq!(branch_fix_rounds[2], (3, Model::Sonnet));
        assert_eq!(branch_fix_rounds[3], (4, Model::Opus));
    }

    // -----------------------------------------------------------------------
    // Recovery re-decomposition tests
    // -----------------------------------------------------------------------

    fn incremental_recovery_plan() -> RecoveryPlan {
        RecoveryPlan {
            full_redecomposition: false,
            subtasks: vec![SubtaskSpec {
                goal: "recovery fix".into(),
                verification_criteria: vec!["fix works".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            }],
            rationale: "incremental recovery".into(),
        }
    }

    fn full_recovery_plan() -> RecoveryPlan {
        RecoveryPlan {
            full_redecomposition: true,
            subtasks: vec![SubtaskSpec {
                goal: "full redo".into(),
                verification_criteria: vec!["redo works".into()],
                magnitude_estimate: MagnitudeEstimate::Medium,
            }],
            rationale: "full re-decomposition".into(),
        }
    }

    /// Child A fails → incremental recovery → recovery subtask succeeds → child B runs → success.
    #[tokio::test]
    async fn recovery_incremental_creates_subtasks() {
        let mock = MockAgentService::new();

        // Root decomposes into 2 children.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Child A: assessed as leaf, fails terminally (9 attempts).
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("A failed"));
        }

        // Recovery: assess says recoverable, plan is incremental.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("retry differently".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery subtask: assessed as leaf, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Child B (still pending, runs after recovery): assessed as leaf, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        // Original 2 children + 1 recovery subtask = 3.
        assert_eq!(root.subtask_ids.len(), 3);
        assert_eq!(root.recovery_rounds, 1);

        // Child A should be Failed.
        let first_child_id = root.subtask_ids[0];
        assert_eq!(
            orch.state.get(first_child_id).unwrap().phase,
            TaskPhase::Failed
        );

        // Child B (pending sibling) should have completed after recovery.
        let second_child_id = root.subtask_ids[1];
        assert_eq!(
            orch.state.get(second_child_id).unwrap().phase,
            TaskPhase::Completed
        );

        // Recovery subtask should have completed and not be marked is_fix_task.
        let recovery_id = root.subtask_ids[2];
        let recovery_task = orch.state.get(recovery_id).unwrap();
        assert_eq!(recovery_task.phase, TaskPhase::Completed);
        assert!(!recovery_task.is_fix_task);
    }

    /// Child A fails → full recovery → pending child B skipped → recovery subtask runs → success.
    #[tokio::test]
    async fn recovery_full_redecomposition_skips_pending() {
        let mock = MockAgentService::new();

        // Root decomposes into 2 children.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Child A: assessed as leaf, fails terminally.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("A failed"));
        }

        // Recovery: full re-decomposition.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("redo everything".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(full_recovery_plan());

        // Recovery subtask: assessed as leaf, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.subtask_ids.len(), 3); // A, B, recovery
        assert_eq!(root.recovery_rounds, 1);

        // Child B should be Failed (superseded).
        let child_b_id = root.subtask_ids[1];
        assert_eq!(orch.state.get(child_b_id).unwrap().phase, TaskPhase::Failed);
    }

    /// Recovery rounds exhausted (2 rounds) → parent fails.
    #[tokio::test]
    async fn recovery_round_limit_exhausted() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "child A".into(),
                    verification_criteria: vec!["A passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "one subtask".into(),
            });

        // Child A fails terminally.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("A failed"));
        }

        // Round 1: recovery with incremental plan.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("try again".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery subtask 1: fails terminally.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("recovery 1 failed"));
        }

        // Round 2: another recovery attempt.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("try again".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery subtask 2: also fails terminally.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("recovery 2 failed"));
        }

        // Round 3 would exceed limit — no more recovery responses needed.

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(
            matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted"))
        );
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 2);
    }

    /// Fix tasks do not attempt recovery (prevents recursive recovery chains).
    /// A fix task that is a branch with a failing child should propagate failure
    /// without calling `assess_recovery`.
    #[tokio::test]
    async fn recovery_not_attempted_for_fix_tasks() {
        // Directly test attempt_recovery by creating a task marked is_fix_task=true.
        let mock = MockAgentService::new();
        let mut state = EpicState::new();

        let root_id = state.next_task_id();
        let mut root = Task::new(root_id, None, "fix parent".into(), vec!["passes".into()], 0);
        root.is_fix_task = true;
        root.path = Some(TaskPath::Branch);
        state.insert(root);

        let (tx, _rx) = events::event_channel();
        let mut orch = Orchestrator::new(mock, state, tx);

        // attempt_recovery should return Some(Failed) immediately for fix tasks.
        let result = orch.attempt_recovery(root_id, "child broke").await.unwrap();
        assert!(result.is_some());
        assert!(matches!(result.unwrap(), TaskOutcome::Failed { .. }));
    }

    /// `assess_recovery` returns None → child failure propagates immediately.
    #[tokio::test]
    async fn recovery_not_attempted_when_unrecoverable() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("terminal"));
        }

        // Recovery assessment: not recoverable.
        mock.recovery_responses.lock().unwrap().push_back(None);

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 0);
    }

    /// Recovery round counter persists across resume.
    #[tokio::test]
    async fn recovery_rounds_persisted() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("A failed"));
        }

        // Round 1 recovery.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("try again".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery subtask succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Verify recovery_rounds is persisted on the task.
        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.recovery_rounds, 1);

        // Verify serde round-trip preserves recovery_rounds.
        let json = serde_json::to_string(&root).unwrap();
        let restored: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.recovery_rounds, 1);
    }

    /// Recovery plan with empty subtask list → treated as failed recovery.
    #[tokio::test]
    async fn recovery_empty_plan_fails() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("broke"));
        }

        // Recovery: assess says recoverable, but plan has no subtasks.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("try something".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(RecoveryPlan {
                full_redecomposition: false,
                subtasks: vec![],
                rationale: "empty plan".into(),
            });

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { reason } if reason.contains("no subtasks")));
        // Round was consumed even though plan was empty.
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 1);
    }

    /// Full re-decomposition with child A completed and child B pending:
    /// child A stays Completed, child B gets Failed (superseded).
    #[tokio::test]
    async fn recovery_full_redecomp_preserves_completed_siblings() {
        let mock = MockAgentService::new();

        // Root decomposes into 3 children.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child C".into(),
                        verification_criteria: vec!["C passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "three subtasks".into(),
            });

        // Child A: succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Child B: fails terminally.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("B failed"));
        }

        // Recovery: full re-decomposition.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("redo".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(full_recovery_plan());

        // Recovery subtask: succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        // A, B, C (original) + recovery = 4.
        assert_eq!(root.subtask_ids.len(), 4);

        // A: Completed (untouched by full re-decomposition).
        assert_eq!(
            orch.state.get(root.subtask_ids[0]).unwrap().phase,
            TaskPhase::Completed
        );
        // B: Failed (the one that triggered recovery).
        assert_eq!(
            orch.state.get(root.subtask_ids[1]).unwrap().phase,
            TaskPhase::Failed
        );
        // C: Failed (superseded — was Pending when full re-decomposition ran).
        assert_eq!(
            orch.state.get(root.subtask_ids[2]).unwrap().phase,
            TaskPhase::Failed
        );
        // Recovery subtask: Completed.
        assert_eq!(
            orch.state.get(root.subtask_ids[3]).unwrap().phase,
            TaskPhase::Completed
        );
    }

    /// Recovery events are emitted correctly.
    #[tokio::test]
    async fn recovery_emits_events() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("broke"));
        }

        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("fix it".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery subtask succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Drain events and check for recovery-specific events.
        let mut saw_recovery_started = false;
        let mut saw_recovery_plan = false;
        let mut saw_recovery_subtasks = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::RecoveryStarted { task_id, round } => {
                    assert_eq!(task_id, root_id);
                    assert_eq!(round, 1);
                    saw_recovery_started = true;
                }
                Event::RecoveryPlanSelected {
                    task_id,
                    ref approach,
                } => {
                    assert_eq!(task_id, root_id);
                    assert_eq!(approach, "incremental");
                    saw_recovery_plan = true;
                }
                Event::RecoverySubtasksCreated {
                    task_id,
                    count,
                    round,
                } => {
                    assert_eq!(task_id, root_id);
                    assert_eq!(count, 1);
                    assert_eq!(round, 1);
                    saw_recovery_subtasks = true;
                }
                _ => {}
            }
        }
        assert!(saw_recovery_started);
        assert!(saw_recovery_plan);
        assert!(saw_recovery_subtasks);
    }

    /// Checkpoint adjust: guidance stored on parent, visible to sibling B via context.
    #[tokio::test]
    async fn checkpoint_adjust_stores_guidance() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Both assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["use API v2".into()],
        });

        // Checkpoint returns Adjust with guidance.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Adjust {
                guidance: "switch to API v2 format".into(),
            });

        // Child B succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child A, child B, root — all pass.
        for _ in 0..3 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Guidance stored on root (the branch parent).
        let root = orch.state.get(root_id).unwrap();
        assert_eq!(
            root.checkpoint_guidance.as_deref(),
            Some("switch to API v2 format")
        );

        // CheckpointAdjust event emitted.
        let mut saw_adjust = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::CheckpointAdjust { task_id } if task_id == root_id) {
                saw_adjust = true;
            }
        }
        assert!(saw_adjust, "CheckpointAdjust event not found");
    }

    /// Checkpoint escalate: triggers recovery machinery.
    #[tokio::test]
    async fn checkpoint_escalate_triggers_recovery() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Child A assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["approach is wrong".into()],
        });

        // Checkpoint returns Escalate.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Escalate);

        // Recovery assess: recoverable.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("switch approach".into()));

        // Recovery plan: incremental, one new subtask.
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(RecoveryPlan {
                full_redecomposition: false,
                subtasks: vec![SubtaskSpec {
                    goal: "recovery child".into(),
                    verification_criteria: vec!["recovery passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "fix approach".into(),
            });

        // Recovery child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Recovery child succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child B (still pending, runs after recovery in incremental mode) assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child B succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child A, recovery child, child B, root — all pass.
        for _ in 0..4 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 4);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Recovery round consumed.
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 1);

        // CheckpointEscalate event emitted.
        let mut saw_escalate = false;
        let mut saw_recovery_started = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
                saw_escalate = true;
            }
            if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
                saw_recovery_started = true;
            }
        }
        assert!(saw_escalate, "CheckpointEscalate event not found");
        assert!(
            saw_recovery_started,
            "RecoveryStarted event not found (escalation should trigger recovery)"
        );
    }

    /// Checkpoint escalate when recovery is not possible: propagates failure.
    #[tokio::test]
    async fn checkpoint_escalate_unrecoverable_fails() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Child A assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["fatal issue".into()],
        });

        // Verification: child A passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Checkpoint returns Escalate.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Escalate);

        // Recovery assess: not recoverable.
        mock.recovery_responses.lock().unwrap().push_back(None);

        queue_file_level_reviews(&mock, 1);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));
    }

    /// Checkpoint agent error treated as Proceed (best-effort).
    #[tokio::test]
    async fn checkpoint_agent_error_treated_as_proceed() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child succeeds with discoveries (triggers checkpoint).
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["something interesting".into()],
        });

        // Inject an error so the checkpoint agent call returns Err.
        mock.checkpoint_errors
            .lock()
            .unwrap()
            .push_back("simulated LLM failure".into());

        // Verification: child, root — both pass.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // The error fallback means no CheckpointAdjust or CheckpointEscalate events.
        while let Ok(event) = rx.try_recv() {
            assert!(
                !matches!(event, Event::CheckpointAdjust { .. }),
                "unexpected CheckpointAdjust event after agent error"
            );
            assert!(
                !matches!(event, Event::CheckpointEscalate { .. }),
                "unexpected CheckpointEscalate event after agent error"
            );
        }

        // No checkpoint_guidance should be stored on the parent.
        assert!(
            orch.state
                .get(root_id)
                .unwrap()
                .checkpoint_guidance
                .is_none(),
            "checkpoint_guidance should be None when agent errors out"
        );
    }

    /// Checkpoint guidance persisted and survives serialization round-trip.
    #[tokio::test]
    async fn checkpoint_guidance_persisted() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A ok".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B ok".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["found issue".into()],
        });

        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Adjust {
                guidance: "use new approach".into(),
            });

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        for _ in 0..3 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Verify guidance survives JSON round-trip.
        let json = serde_json::to_string(&orch.state).unwrap();
        let restored: EpicState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored
                .get(root_id)
                .unwrap()
                .checkpoint_guidance
                .as_deref(),
            Some("use new approach")
        );
    }

    #[tokio::test]
    async fn checkpoint_multiple_adjusts_accumulates_guidance() {
        let mock = MockAgentService::new();

        // Root decomposes into 3 children: A, B, C.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A ok".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B ok".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child C".into(),
                        verification_criteria: vec!["C ok".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "three subtasks".into(),
            });

        // All three assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries → triggers checkpoint.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["discovered API v2".into()],
        });

        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Adjust {
                guidance: "use API v2".into(),
            });

        // Child B succeeds with discoveries → triggers checkpoint.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["discovered gzip support".into()],
        });

        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Adjust {
                guidance: "also use gzip".into(),
            });

        // Child C succeeds without discoveries → no checkpoint.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // 4 verifications: children A, B, C + root.
        for _ in 0..4 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 4);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Guidance accumulates newline-separated rather than being overwritten.
        assert_eq!(
            orch.state
                .get(root_id)
                .unwrap()
                .checkpoint_guidance
                .as_deref(),
            Some("use API v2\nalso use gzip")
        );

        // Two CheckpointAdjust events emitted.
        let mut adjust_count = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::CheckpointAdjust { task_id } if task_id == root_id) {
                adjust_count += 1;
            }
        }
        assert_eq!(
            adjust_count, 2,
            "expected exactly 2 CheckpointAdjust events"
        );
    }

    /// When a fix task's child discoveries trigger Escalate, recovery is rejected
    /// because `attempt_recovery` refuses fix tasks, so the branch fails immediately.
    #[tokio::test]
    async fn checkpoint_escalate_on_fix_task_fails() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Child A assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries → triggers checkpoint.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["fatal issue".into()],
        });

        // Verification: child A passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Checkpoint returns Escalate.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Escalate);

        // No recovery_responses needed — attempt_recovery rejects fix tasks before
        // consulting the agent.

        queue_file_level_reviews(&mock, 1);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);

        // Mark the root as a fix task so attempt_recovery rejects it.
        orch.state.get_mut(root_id).unwrap().is_fix_task = true;

        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        // Verify events: CheckpointEscalate emitted, RecoveryStarted not emitted.
        let mut saw_escalate = false;
        let mut saw_recovery_started = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
                saw_escalate = true;
            }
            if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
                saw_recovery_started = true;
            }
        }
        assert!(saw_escalate, "CheckpointEscalate event not found");
        assert!(
            !saw_recovery_started,
            "RecoveryStarted should not be emitted for fix tasks"
        );
    }

    #[tokio::test]
    async fn checkpoint_guidance_flows_to_child_context() {
        let mock = MockAgentService::new();
        let (tx, _rx) = events::event_channel();

        let mut state = EpicState::new();

        // Create root (branch) with two children A and B.
        let root_id = state.next_task_id();
        let mut root = Task::new(
            root_id,
            None,
            "root goal".into(),
            vec!["root passes".into()],
            0,
        );

        let first_child_id = state.next_task_id();
        let child_a = Task::new(
            first_child_id,
            Some(root_id),
            "child A goal".into(),
            vec!["A passes".into()],
            1,
        );

        let second_child_id = state.next_task_id();
        let child_b = Task::new(
            second_child_id,
            Some(root_id),
            "child B goal".into(),
            vec!["B passes".into()],
            1,
        );

        root.subtask_ids = vec![first_child_id, second_child_id];
        root.checkpoint_guidance = Some("use API v2".into());
        state.insert(root);

        // Child A is completed.
        let mut a = child_a;
        a.phase = TaskPhase::Completed;
        state.insert(a);

        // Child B is pending.
        state.insert(child_b);

        let orch = Orchestrator::new(mock, state, tx);
        let ctx = orch.build_context(second_child_id).unwrap();

        assert_eq!(
            ctx.checkpoint_guidance.as_deref(),
            Some("use API v2"),
            "checkpoint guidance from parent should flow into child context"
        );
    }

    /// Checkpoint escalation when recovery rounds are already at `max_recovery_rounds`
    /// results in immediate failure without starting a new recovery round.
    #[tokio::test]
    async fn checkpoint_escalate_recovery_rounds_exhausted() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Child A assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["approach is wrong".into()],
        });

        // Verification: child A passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Checkpoint returns Escalate.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Escalate);

        // No recovery_responses needed — attempt_recovery will bail out
        // before calling assess_recovery because recovery_rounds >= max_recovery_rounds.

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);

        // Pre-set recovery_rounds to max_recovery_rounds so escalation exhausts immediately.
        orch.state.get_mut(root_id).unwrap().recovery_rounds = 2;

        let result = orch.run(root_id).await.unwrap();
        assert!(
            matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("recovery rounds exhausted")),
            "expected failure with 'recovery rounds exhausted', got: {result:?}"
        );

        // Verify events.
        let mut saw_escalate = false;
        let mut saw_recovery_started = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
                saw_escalate = true;
            }
            if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
                saw_recovery_started = true;
            }
        }
        assert!(saw_escalate, "CheckpointEscalate event not found");
        assert!(
            !saw_recovery_started,
            "RecoveryStarted should not be emitted when recovery rounds are exhausted"
        );
    }

    /// Checkpoint escalate after prior adjust: guidance is cleared before recovery runs.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn checkpoint_escalate_clears_prior_guidance() {
        let mock = MockAgentService::new();

        // Root decomposes into 3 children: A, B, C.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child C".into(),
                        verification_criteria: vec!["C passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "three subtasks".into(),
            });

        // Child A assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child A succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["use API v2".into()],
        });

        // Checkpoint returns Adjust with guidance.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Adjust {
                guidance: "old guidance".into(),
            });

        // Child B assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child B succeeds with discoveries.
        mock.leaf_responses.lock().unwrap().push_back(LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["approach is fundamentally wrong".into()],
        });

        // Checkpoint returns Escalate.
        mock.checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Escalate);

        // Recovery assess: recoverable.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("fix approach".into()));

        // Recovery plan: incremental, one new subtask.
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(RecoveryPlan {
                full_redecomposition: false,
                subtasks: vec![SubtaskSpec {
                    goal: "recovery child".into(),
                    verification_criteria: vec!["recovery passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "fix approach".into(),
            });

        // Recovery child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Recovery child succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child C (still pending in incremental mode) assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child C succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child A, child B, recovery child, child C, root — all pass.
        for _ in 0..5 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 4);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Guidance cleared by escalation.
        assert!(
            orch.state
                .get(root_id)
                .unwrap()
                .checkpoint_guidance
                .is_none(),
            "checkpoint_guidance should be None after escalation clears prior adjust guidance"
        );

        // Both CheckpointAdjust and CheckpointEscalate events emitted.
        let mut saw_adjust = false;
        let mut saw_escalate = false;
        let mut saw_recovery_started = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::CheckpointAdjust { task_id } if task_id == root_id) {
                saw_adjust = true;
            }
            if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
                saw_escalate = true;
            }
            if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
                saw_recovery_started = true;
            }
        }
        assert!(saw_adjust, "CheckpointAdjust event not found");
        assert!(saw_escalate, "CheckpointEscalate event not found");
        assert!(
            saw_recovery_started,
            "RecoveryStarted event not found (escalation should trigger recovery)"
        );

        // Recovery round consumed.
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 1);
    }

    /// Resume mid-leaf-retry: pre-existing attempts are counted so `retries_at_tier` is correct.
    #[tokio::test]
    async fn leaf_retry_counter_persists_on_resume() {
        let mock = MockAgentService::new();

        // Child already assessed and mid-execution with 2 failed Haiku attempts persisted.
        // On resume, retries_at_tier should start at 2. One more failure should escalate to Sonnet
        // (not grant a fresh 3 retries).

        // Next attempt (Haiku, attempt #3) fails → triggers escalation to Sonnet.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_failed("fail3"));

        // First Sonnet attempt succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child".into(),
            vec!["child passes".into()],
            1,
        );
        child.path = Some(TaskPath::Leaf);
        child.current_model = Some(Model::Haiku);
        child.phase = TaskPhase::Executing;
        child.attempts = vec![
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("fail1".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("fail2".into()),
            },
        ];

        state.insert(root);
        state.insert(child);

        let (tx, mut rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        // 2 pre-existing + 1 failed Haiku + 1 successful Sonnet = 4 total.
        assert_eq!(child.attempts.len(), 4);
        assert_eq!(child.attempts[2].model, Model::Haiku);
        assert!(!child.attempts[2].succeeded);
        assert_eq!(child.attempts[3].model, Model::Sonnet);
        assert!(child.attempts[3].succeeded);

        // Verify escalation event was emitted (Haiku → Sonnet).
        let mut saw_escalation = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event,
                Event::ModelEscalated {
                    from: Model::Haiku,
                    to: Model::Sonnet,
                    ..
                }
            ) {
                saw_escalation = true;
            }
        }
        assert!(
            saw_escalation,
            "ModelEscalated event should be emitted after 3 Haiku failures"
        );
    }

    /// Resume at Sonnet tier with pre-existing Sonnet attempts: `retries_at_tier` counts
    /// only trailing Sonnet attempts, not prior Haiku attempts.
    #[tokio::test]
    async fn leaf_retry_counter_resume_at_sonnet_tier() {
        let mock = MockAgentService::new();

        // Child has 3 Haiku failures + 2 Sonnet failures. On resume, retries_at_tier
        // should be 2 (only the trailing Sonnet attempts). One more Sonnet failure
        // should escalate to Opus.

        // Sonnet attempt #3 fails → escalation to Opus.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_failed("sonnet fail3"));

        // Opus attempt #1 succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child".into(),
            vec!["child passes".into()],
            1,
        );
        child.path = Some(TaskPath::Leaf);
        child.current_model = Some(Model::Sonnet);
        child.phase = TaskPhase::Executing;
        child.attempts = vec![
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("h1".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("h2".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("h3".into()),
            },
            Attempt {
                model: Model::Sonnet,
                succeeded: false,
                error: Some("s1".into()),
            },
            Attempt {
                model: Model::Sonnet,
                succeeded: false,
                error: Some("s2".into()),
            },
        ];

        state.insert(root);
        state.insert(child);

        let (tx, mut rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        // 5 pre-existing + 1 failed Sonnet + 1 successful Opus = 7 total.
        assert_eq!(child.attempts.len(), 7);
        assert_eq!(child.attempts[5].model, Model::Sonnet);
        assert!(!child.attempts[5].succeeded);
        assert_eq!(child.attempts[6].model, Model::Opus);
        assert!(child.attempts[6].succeeded);

        // Verify escalation Sonnet → Opus.
        let mut saw_escalation = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event,
                Event::ModelEscalated {
                    from: Model::Sonnet,
                    to: Model::Opus,
                    ..
                }
            ) {
                saw_escalation = true;
            }
        }
        assert!(saw_escalation, "ModelEscalated Sonnet→Opus event expected");
    }

    /// Resume with retries exhausted at current tier: escalates immediately without
    /// executing an extra attempt (crash between recording failure and escalation).
    #[tokio::test]
    async fn leaf_retry_resume_escalates_immediately_when_tier_exhausted() {
        let mock = MockAgentService::new();

        // Child has 3 Haiku failures (tier exhausted) but current_model is still Haiku
        // (crash happened before escalation). Should escalate to Sonnet without executing
        // a 4th Haiku attempt.

        // First Sonnet attempt succeeds (no Haiku attempts should be made).
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let child_id = state.next_task_id();

        let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
        root.path = Some(TaskPath::Branch);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child".into(),
            vec!["child passes".into()],
            1,
        );
        child.path = Some(TaskPath::Leaf);
        child.current_model = Some(Model::Haiku); // Not yet escalated.
        child.phase = TaskPhase::Executing;
        child.attempts = vec![
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("f1".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("f2".into()),
            },
            Attempt {
                model: Model::Haiku,
                succeeded: false,
                error: Some("f3".into()),
            },
        ];

        state.insert(root);
        state.insert(child);

        let (tx, mut rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        // 3 pre-existing Haiku + 1 successful Sonnet = 4 (no extra Haiku attempt).
        assert_eq!(child.attempts.len(), 4);
        assert_eq!(child.attempts[3].model, Model::Sonnet);
        assert!(child.attempts[3].succeeded);

        // Verify escalation event was emitted.
        let mut saw_escalation = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event,
                Event::ModelEscalated {
                    from: Model::Haiku,
                    to: Model::Sonnet,
                    ..
                }
            ) {
                saw_escalation = true;
            }
        }
        assert!(
            saw_escalation,
            "ModelEscalated Haiku→Sonnet expected on immediate escalation"
        );
    }

    /// Leaf retry attempts are persisted to disk via `checkpoint_save`.
    #[tokio::test]
    async fn leaf_retry_attempts_persisted_to_disk() {
        let mock = MockAgentService::new();

        // Root decomposes into 1 child.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child fails once, then succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_failed("first try failed"));
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child passes, root passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state_path = tmp.path().to_path_buf();

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.state_path = Some(state_path.clone());
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // The child's attempts are persisted (2 attempts: 1 failed + 1 succeeded).
        let loaded_state = EpicState::load(&state_path).unwrap();
        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = loaded_state.get(child_id).unwrap();
        assert_eq!(child.attempts.len(), 2);
        assert!(!child.attempts[0].succeeded);
        assert!(child.attempts[1].succeeded);
    }

    // -----------------------------------------------------------------------
    // Config wiring tests: verify non-default config values change behavior
    // -----------------------------------------------------------------------

    /// Custom `max_depth`=2: root at depth 1, child at depth 2 is forced to Leaf without assess.
    #[tokio::test]
    async fn custom_max_depth_forces_leaf() {
        let mock = MockAgentService::new();

        // Root branches (forced), decomposition returns 1 subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // No assess response queued — child should be force-leafed without calling assess.

        // Child leaf execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let limits = LimitsConfig {
            max_depth: 2,
            ..LimitsConfig::default()
        };

        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let root = Task::new(root_id, None, "deep root".into(), vec!["passes".into()], 1);
        state.insert(root);
        let (tx, _rx) = events::event_channel();
        queue_file_level_reviews(&mock, 2);
        let mut orch = Orchestrator::new(mock, state, tx).with_limits(limits);

        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.path, Some(TaskPath::Leaf));
        assert_eq!(child.depth, 2);
    }

    /// Custom `retry_budget`=1: Haiku fails once → immediately escalates to Sonnet.
    #[tokio::test]
    async fn custom_retry_budget_escalates_early() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // 1 Haiku failure → escalate → 1 Sonnet success.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_failed("haiku failed"));
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let limits = LimitsConfig {
            retry_budget: 1,
            ..LimitsConfig::default()
        };

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;

        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        // Only 2 attempts total: 1 Haiku fail + 1 Sonnet success.
        assert_eq!(child.attempts.len(), 2);
        assert_eq!(child.current_model, Some(Model::Sonnet));
    }

    /// Custom `max_recovery_rounds`=1: recovery attempted once, refused on second failure.
    #[tokio::test]
    async fn custom_max_recovery_rounds_limits_recovery() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "child A".into(),
                    verification_criteria: vec!["A passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "one subtask".into(),
            });

        // Child A fails terminally (9 attempts: 3 per tier).
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("A failed"));
        }

        // Round 1: recovery with incremental plan.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("try again".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery subtask 1: also fails terminally.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("recovery failed"));
        }

        // Round 2 would exceed limit (max_recovery_rounds=1) — no more recovery responses needed.

        let limits = LimitsConfig {
            max_recovery_rounds: 1,
            ..LimitsConfig::default()
        };

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;

        let result = orch.run(root_id).await.unwrap();
        assert!(
            matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted"))
        );
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 1);
    }

    /// Custom `root_fix_rounds`=1: root verification fails → 1 fix round → still fails → task fails.
    #[tokio::test]
    async fn custom_root_fix_rounds_limits_fix_attempts() {
        let mock = MockAgentService::new();

        // Root branches, 1 original subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Original child: leaf, succeeds, verification passes.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root check failed"));

        // 1 fix round: fix subtask created, executed (leaf, succeeds, verification passes).
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root re-verification still fails after round 1.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root still failing"));

        // With root_fix_rounds=1, no more rounds allowed — task fails.

        let limits = LimitsConfig {
            root_fix_rounds: 1,
            ..LimitsConfig::default()
        };

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;

        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.verification_fix_rounds, 1);
        assert_eq!(root.phase, TaskPhase::Failed);
    }

    /// Custom `branch_fix_rounds`=1: non-root branch verification fails → 1 fix round → fails.
    #[tokio::test]
    async fn custom_branch_fix_rounds_limits_fix_attempts() {
        let mock = MockAgentService::new();

        // Root branches into 1 child.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as Branch (not leaf).
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            });

        // Child decomposes into 1 grandchild.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "grandchild".into(),
                    verification_criteria: vec!["gc passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "one grandchild".into(),
            });

        // Grandchild: leaf, succeeds, verification passes.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Child (branch) verification fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("branch check failed"));

        // 1 fix round for child branch.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Child re-verification still fails.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("branch still failing"));

        // Child fails with branch_fix_rounds=1.
        // Root: recovery for child failure — not recoverable.
        mock.recovery_responses.lock().unwrap().push_back(None);

        let limits = LimitsConfig {
            branch_fix_rounds: 1,
            ..LimitsConfig::default()
        };

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;

        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.verification_fix_rounds, 1);
        assert_eq!(child.phase, TaskPhase::Failed);
    }

    /// `retry_budget`=0 is clamped to 1: leaf still gets at least one attempt.
    #[tokio::test]
    async fn zero_retry_budget_clamped_to_one() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // 1 Haiku failure → escalate (budget=1) → 1 Sonnet success.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_failed("haiku failed"));
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let limits = LimitsConfig {
            retry_budget: 0, // Should be clamped to 1.
            ..LimitsConfig::default()
        };

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;

        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        // 2 attempts: 1 Haiku fail + 1 Sonnet success (same as retry_budget=1).
        assert_eq!(child.attempts.len(), 2);
        assert_eq!(child.current_model, Some(Model::Sonnet));
    }

    /// Leaf with Haiku model passes Haiku to verify.
    #[tokio::test]
    async fn verify_model_leaf_haiku() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Leaf,
                model: Model::Haiku,
                rationale: "simple".into(),
                magnitude: None,
            });

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child verify + root verify.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let captured = orch.services.agent.verify_models.lock().unwrap().clone();
        // First verify call is child (leaf Haiku), second is root (branch Sonnet).
        assert_eq!(captured[0], Model::Haiku);
    }

    /// Leaf with Sonnet model passes Sonnet to verify.
    #[tokio::test]
    async fn verify_model_leaf_sonnet() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Leaf,
                model: Model::Sonnet,
                rationale: "medium".into(),
                magnitude: None,
            });

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let captured = orch.services.agent.verify_models.lock().unwrap().clone();
        assert_eq!(captured[0], Model::Sonnet);
    }

    /// Leaf with Opus model gets capped to Sonnet for verification.
    #[tokio::test]
    async fn verify_model_leaf_opus_capped() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: TaskPath::Leaf,
                model: Model::Opus,
                rationale: "hard leaf".into(),
                magnitude: None,
            });

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let captured = orch.services.agent.verify_models.lock().unwrap().clone();
        // Opus is clamped to Sonnet for leaf verification.
        assert_eq!(captured[0], Model::Sonnet);
    }

    /// Branch task always uses Sonnet for verification.
    #[tokio::test]
    async fn verify_model_branch_always_sonnet() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let captured = orch.services.agent.verify_models.lock().unwrap().clone();
        // Second verify call is for root (branch) — always Sonnet.
        assert_eq!(captured[1], Model::Sonnet);
    }

    /// Branch decompose receives the model from assessment.
    #[tokio::test]
    async fn decompose_model_from_assessment() {
        let mock = MockAgentService::new();

        // Root always branches with Sonnet (hardcoded in execute_task).
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let captured = orch.services.agent.decompose_models.lock().unwrap().clone();
        // Root's assessment is hardcoded to Model::Sonnet.
        assert_eq!(captured[0], Model::Sonnet);
    }

    // ---- Task limit cap tests ----

    /// Decomposition fails gracefully when total task limit would be exceeded.
    #[tokio::test]
    async fn task_limit_blocks_decomposition() {
        let mock = MockAgentService::new();

        // Decompose into 2 subtasks.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        // Set limit so tight that 1 existing + 2 new > 2.
        orch.services.limits.max_total_tasks = 2;
        let result = orch.run(root_id).await.unwrap();
        let TaskOutcome::Failed { reason } = &result else {
            panic!("expected TaskOutcome::Failed, got {result:?}");
        };
        assert!(
            reason.contains("task limit reached"),
            "unexpected reason: {reason}"
        );

        // Drain events and assert exactly one TaskLimitReached with the correct task_id.
        let mut limit_events: Vec<TaskId> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let Event::TaskLimitReached { task_id } = event {
                limit_events.push(task_id);
            }
        }
        assert_eq!(
            limit_events.len(),
            1,
            "expected exactly one TaskLimitReached event"
        );
        assert_eq!(limit_events[0], root_id);
    }

    /// Fix subtask creation blocked by task limit (`branch_fix_loop` path).
    #[tokio::test]
    async fn task_limit_blocks_fix_subtasks() {
        // Execution flow:
        // 1. Root auto-assessed as Branch (depth 0)
        // 2. Root decomposes into 1 child
        // 3. Child assessed as Leaf, executes, succeeds
        // 4. Child verification passes
        // 5. Root verification fails → enters branch_fix_loop
        // 6. Fix agent designs 1 fix subtask → task limit blocks creation

        let mock = MockAgentService::new();

        // Root decomposes into 1 child (leaf) that succeeds.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child leaf verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root (branch) verification fails → triggers branch_fix_loop on root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(VerificationResult {
                outcome: VerificationOutcome::Fail {
                    reason: "root verification failed".into(),
                },
                details: "check failed".into(),
            });

        // branch_fix_loop designs 1 fix subtask.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "fix child".into(),
                    verification_criteria: vec!["fix passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "fix".into(),
            });

        queue_file_level_reviews(&mock, 1);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        // root(1) + child(2) = 2 tasks. Fix would add a 3rd. Set limit to 2.
        orch.services.limits.max_total_tasks = 2;
        let result = orch.run(root_id).await.unwrap();
        let TaskOutcome::Failed { reason } = &result else {
            panic!("expected TaskOutcome::Failed, got {result:?}");
        };
        assert!(
            reason.contains("task limit reached"),
            "unexpected reason: {reason}"
        );
    }

    /// Recovery subtask creation blocked by task limit.
    #[tokio::test]
    async fn task_limit_blocks_recovery_subtasks() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        // Child fails.
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("boom"));
        }

        // Recovery is possible.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("retry with different approach".into()));

        // Recovery plan produces 1 subtask.
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(RecoveryPlan {
                full_redecomposition: false,
                subtasks: vec![SubtaskSpec {
                    goal: "recovery child".into(),
                    verification_criteria: vec!["recovers".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "recovery".into(),
            });

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        // root(1) + child(2) = 2 tasks. Recovery would add 3rd. Set limit to 2.
        orch.services.limits.max_total_tasks = 2;
        let result = orch.run(root_id).await.unwrap();
        let TaskOutcome::Failed { reason } = &result else {
            panic!("expected TaskOutcome::Failed, got {result:?}");
        };
        assert!(
            reason.contains("task limit reached"),
            "unexpected reason: {reason}"
        );
    }

    /// Recovery subtasks inherit parent's `recovery_rounds` (no fresh budget).
    #[tokio::test]
    async fn recovery_depth_inherited_not_fresh() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        // Child fails all tiers.
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("boom"));
        }

        // Recovery round 1: assess as recoverable.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("retry".into()));

        // Recovery plan creates 1 subtask (a branch that will itself decompose).
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(RecoveryPlan {
                full_redecomposition: false,
                subtasks: vec![SubtaskSpec {
                    goal: "recovery branch".into(),
                    verification_criteria: vec!["recovers".into()],
                    magnitude_estimate: MagnitudeEstimate::Small,
                }],
                rationale: "recovery".into(),
            });

        // Recovery child assessed as leaf, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        // Verification: recovery child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits.max_total_tasks = 100;
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // The recovery child should have inherited parent's recovery_rounds (1),
        // not started at 0.
        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.recovery_rounds, 1);
        // Find the recovery child (should be the last subtask).
        let recovery_child_id = *root.subtask_ids.last().unwrap();
        let recovery_child = orch.state.get(recovery_child_id).unwrap();
        assert_eq!(
            recovery_child.recovery_rounds, 1,
            "recovery subtask should inherit parent's recovery_rounds, not start at 0"
        );
    }

    /// Inherited recovery budget blocks a second recovery round.
    ///
    /// With `max_recovery_rounds = 1`, a recovery child inherits `recovery_rounds = 1`
    /// from its parent. When the recovery child also fails terminally, the parent
    /// attempts a second recovery round but is denied because its counter (1) already
    /// meets the limit (1). The run therefore fails.
    #[tokio::test]
    async fn recovery_inherited_budget_blocks_second_recovery() {
        let mock = MockAgentService::new();

        // Root decomposes into 1 child.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf, fails all tiers (9 failures).
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("child failed"));
        }

        // Recovery round 1: assess as recoverable, create 1 recovery subtask.
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(Some("retry".into()));
        mock.recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(incremental_recovery_plan());

        // Recovery child assessed as leaf, fails all tiers (9 failures).
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_failed("recovery child failed"));
        }

        // No second recovery response needed: budget exhausted (recovery_rounds=1 >= max=1).

        let limits = LimitsConfig {
            max_recovery_rounds: 1,
            ..LimitsConfig::default()
        };

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;

        let result = orch.run(root_id).await.unwrap();

        // The run must fail — inherited budget prevents a second recovery round.
        assert!(
            matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("recovery rounds exhausted")),
            "expected recovery-exhausted failure, got {result:?}"
        );

        // Root consumed exactly 1 recovery round.
        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.recovery_rounds, 1);

        // Recovery child inherited recovery_rounds = 1 from parent.
        let recovery_child_id = *root.subtask_ids.last().unwrap();
        let recovery_child = orch.state.get(recovery_child_id).unwrap();
        assert_eq!(
            recovery_child.recovery_rounds, 1,
            "recovery child should inherit parent's recovery_rounds (1), blocking further recovery"
        );
    }

    /// `max_total_tasks = 0` is clamped to 1 so a single-leaf run succeeds.
    #[tokio::test]
    async fn max_total_tasks_zero_clamped_blocks_decomposition() {
        let mock = MockAgentService::new();

        // Root is forced to Branch (depth 0) and decomposes into 1 leaf child.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child, root.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits.max_total_tasks = 0;
        // max_total_tasks=0 is clamped to 1. Root (1 task) + 1 child = 2 > 1,
        // so decomposition is still blocked. The key assertion: no panic from
        // a zero limit, and the clamp actually took effect.
        let result = orch.run(root_id).await.unwrap();
        assert!(
            matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("task limit reached")),
            "expected task limit failure after clamping 0→1, got {result:?}"
        );
    }

    /// Exact boundary: `max_total_tasks = 3`, root + 2 children = 3 (not > 3), succeeds.
    #[tokio::test]
    async fn task_limit_exact_boundary_permits() {
        let mock = MockAgentService::new();

        // Root decomposes into 2 children.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(DecompositionResult {
                subtasks: vec![
                    SubtaskSpec {
                        goal: "child A".into(),
                        verification_criteria: vec!["A passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                    SubtaskSpec {
                        goal: "child B".into(),
                        verification_criteria: vec!["B passes".into()],
                        magnitude_estimate: MagnitudeEstimate::Small,
                    },
                ],
                rationale: "two subtasks".into(),
            });

        // Both assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Both succeed.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification: child A, child B, root — all pass.
        for _ in 0..3 {
            mock.verify_responses
                .lock()
                .unwrap()
                .push_back(pass_verification());
        }

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        // root(1) + 2 children = 3, limit = 3 → 3 is NOT > 3 → allowed.
        orch.services.limits.max_total_tasks = 3;
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(orch.state.task_count(), 3);
    }

    /// Leaf fix loop: `verify()` returns Err on first attempt, succeeds on second.
    #[tokio::test]
    async fn leaf_fix_verify_error_retries() {
        let mock = MockAgentService::new();

        // Root branches, 1 subtask.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        // Child assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child execution succeeds.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Initial verification fails (triggers fix loop).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("tests fail"));

        // Fix attempt 1 succeeds, but verify() returns Err.
        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.push_verify_errors(
            TaskId(1),
            vec![
                None, // initial verify uses verify_responses
                Some("transient API error".into()),
            ],
        );

        // Fix attempt 2 succeeds, verify() passes.
        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        assert_eq!(child.fix_attempts.len(), 2);
    }

    /// Branch fix loop: `design_fix_subtasks()` returns Err on round 1, succeeds on round 2.
    #[tokio::test]
    async fn branch_fix_design_error_retries() {
        let mock = MockAgentService::new();
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Round 1: design_fix_subtasks returns Err (consumes the round).
        mock.push_fix_subtask_errors(TaskId(0), vec![Some("LLM timeout".into())]);

        // Round 2: design_fix_subtasks succeeds.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        // Fix subtask: assessed as leaf, executes, succeeds.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Fix subtask verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.phase, TaskPhase::Completed);
        // Round 1 (error) + round 2 (success) = 2 fix rounds consumed.
        assert_eq!(root.verification_fix_rounds, 2);
    }

    /// Branch fix loop: `verify()` returns Err on round 1 re-verification, passes on round 2.
    #[tokio::test]
    async fn branch_fix_verify_error_retries() {
        let mock = MockAgentService::new();
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Round 1: design succeeds, fix subtask succeeds, verify() returns Err.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification()); // fix subtask verification
        mock.push_verify_errors(
            TaskId(0),
            vec![
                None, // initial root verify uses verify_responses
                Some("transient verify error".into()),
            ],
        );

        // Round 2: design succeeds, fix subtask succeeds, verify() passes.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification()); // fix subtask verification
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification()); // root re-verify passes

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.phase, TaskPhase::Completed);
        assert_eq!(root.verification_fix_rounds, 2);
    }

    /// Branch fix loop: mixed error types across rounds.
    /// Round 1: `design_fix_subtasks` Err. Round 2: verify Err. Round 3: success.
    #[tokio::test]
    async fn branch_fix_mixed_errors_then_success() {
        let mock = MockAgentService::new();
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Round 1: design_fix_subtasks returns Err (consumes the round).
        mock.push_fix_subtask_errors(TaskId(0), vec![Some("LLM timeout".into())]);

        // Round 2: design succeeds, fix subtask succeeds, verify() returns Err.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification()); // fix subtask verification
        mock.push_verify_errors(
            TaskId(0),
            vec![
                None, // initial root verify uses verify_responses
                Some("transient verify error".into()),
            ],
        );

        // Round 3: design succeeds, fix subtask succeeds, verify() passes.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification()); // fix subtask verification
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification()); // root re-verify passes

        queue_file_level_reviews(&mock, 3);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.phase, TaskPhase::Completed);
        assert_eq!(root.verification_fix_rounds, 3);
    }

    /// All `root_fix_rounds` consumed by `design_fix_subtasks` errors → Failed.
    #[tokio::test]
    async fn branch_fix_design_error_exhausts_budget() {
        let mock = MockAgentService::new();
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Both rounds: design_fix_subtasks returns Err.
        mock.push_fix_subtask_errors(
            TaskId(0),
            vec![
                Some("LLM timeout round 1".into()),
                Some("LLM timeout round 2".into()),
            ],
        );

        // Recovery assessment for branch failure.
        mock.recovery_responses.lock().unwrap().push_back(None);

        let limits = LimitsConfig {
            root_fix_rounds: 2,
            ..LimitsConfig::default()
        };

        queue_file_level_reviews(&mock, 1);
        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.services.limits = limits;
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.phase, TaskPhase::Failed);
        assert_eq!(root.verification_fix_rounds, 2);
    }

    /// All leaf fix retries across all tiers fail verification → Failed.
    #[tokio::test]
    async fn leaf_fix_verify_error_exhausts_budget() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Initial verification fails (triggers fix loop).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("tests fail"));

        // 3 tiers (Haiku, Sonnet, Opus) × 3 retries = 9 fix attempts.
        // Each fix_leaf succeeds but verify returns Err.
        let child_id = TaskId(1);
        let mut errors: Vec<Option<String>> = vec![None]; // initial verify uses verify_responses
        errors.extend(std::iter::repeat_n(
            Some("persistent verify error".into()),
            9,
        ));
        mock.push_verify_errors(child_id, errors);
        for _ in 0..9 {
            mock.fix_leaf_responses
                .lock()
                .unwrap()
                .push_back(leaf_success());
        }

        // Recovery assessment for leaf failure.
        mock.recovery_responses.lock().unwrap().push_back(None);
        // Recovery for root after child fails.
        mock.recovery_responses.lock().unwrap().push_back(None);

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { .. }));

        let actual_child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(actual_child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Failed);
        assert_eq!(child.fix_attempts.len(), 9);
    }

    /// Initial `verify()` returning `Err` in `finalize_branch` (outside any fix loop)
    /// must propagate as `Err` from `run()`, not be swallowed into `Ok(Failed)`.
    #[tokio::test]
    async fn initial_verify_error_is_fatal() {
        let mock = MockAgentService::new();

        // Root decomposes to 1 leaf child.
        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Child executes successfully.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Initial verify() for child returns Err (agent error, not verification failure).
        mock.push_verify_errors(TaskId(1), vec![Some("agent crashed".into())]);

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await;
        assert!(result.is_err());
    }

    /// When all non-fix children are Failed (e.g. on resume after recovery exhaustion),
    /// `execute_branch` must return Failure, not vacuous Success.
    #[tokio::test]
    async fn branch_fails_when_all_children_failed() {
        let mock = MockAgentService::new();
        let mut state = EpicState::new();

        // Root: branch, mid-execution (simulates resume).
        let root_id = state.next_task_id();
        let mut root = Task::new(
            root_id,
            None,
            "root goal".into(),
            vec!["root passes".into()],
            0,
        );
        root.path = Some(TaskPath::Branch);
        root.phase = TaskPhase::Executing;

        // Two children, both already Failed.
        let child_a = state.next_task_id();
        let mut a = Task::new(
            child_a,
            Some(root_id),
            "child A".into(),
            vec!["A passes".into()],
            1,
        );
        a.phase = TaskPhase::Failed;
        a.path = Some(TaskPath::Leaf);

        let child_b = state.next_task_id();
        let mut b = Task::new(
            child_b,
            Some(root_id),
            "child B".into(),
            vec!["B passes".into()],
            1,
        );
        b.phase = TaskPhase::Failed;
        b.path = Some(TaskPath::Leaf);

        root.subtask_ids = vec![child_a, child_b];
        state.insert(root);
        state.insert(a);
        state.insert(b);

        let (tx, _rx) = events::event_channel();
        let mut orch = Orchestrator::new(mock, state, tx);

        let result = orch.run(root_id).await.unwrap();
        assert!(
            matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("all non-fix children failed")),
            "expected Failure when all children failed, got: {result:?}"
        );
    }

    /// Branch with a mix of failed non-fix children and a successful non-fix child
    /// should still report Success (only fails when ALL non-fix children failed).
    #[tokio::test]
    async fn branch_succeeds_when_some_children_completed() {
        let mock = MockAgentService::new();
        let mut state = EpicState::new();

        let root_id = state.next_task_id();
        let mut root = Task::new(
            root_id,
            None,
            "root goal".into(),
            vec!["root passes".into()],
            0,
        );
        root.path = Some(TaskPath::Branch);
        root.phase = TaskPhase::Executing;

        let child_a = state.next_task_id();
        let mut a = Task::new(
            child_a,
            Some(root_id),
            "child A".into(),
            vec!["A passes".into()],
            1,
        );
        a.phase = TaskPhase::Completed;
        a.path = Some(TaskPath::Leaf);

        let child_b = state.next_task_id();
        let mut b = Task::new(
            child_b,
            Some(root_id),
            "child B".into(),
            vec!["B passes".into()],
            1,
        );
        b.phase = TaskPhase::Failed;
        b.path = Some(TaskPath::Leaf);

        root.subtask_ids = vec![child_a, child_b];
        state.insert(root);
        state.insert(a);
        state.insert(b);

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let (tx, _rx) = events::event_channel();
        let mut orch = Orchestrator::new(mock, state, tx);

        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);
    }

    /// `build_context` populates `parent_decomposition_rationale`, `parent_discoveries`, and `children`.
    #[test]
    fn build_context_populates_parent_fields_and_children() {
        let mut state = EpicState::new();
        let parent_id = state.next_task_id(); // T0
        let child_id = state.next_task_id(); // T1

        let mut parent = Task::new(
            parent_id,
            None,
            "parent goal".into(),
            vec!["parent passes".into()],
            0,
        );
        parent.decomposition_rationale = Some("split by module".into());
        parent.discoveries = vec!["API uses v2".into(), "config moved".into()];
        parent.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(parent_id),
            "child goal".into(),
            vec!["child passes".into()],
            1,
        );
        child.phase = TaskPhase::Completed;
        child.discoveries = vec!["found bug".into()];

        state.insert(parent);
        state.insert(child);

        let mock = MockAgentService::new();
        let (tx, _rx) = events::event_channel();
        let orch = Orchestrator::new(mock, state, tx);

        // Build context for the child — should pull parent fields.
        let ctx = orch.build_context(child_id).unwrap();
        assert_eq!(
            ctx.parent_decomposition_rationale.as_deref(),
            Some("split by module"),
        );
        assert_eq!(ctx.parent_discoveries, vec!["API uses v2", "config moved"]);

        // Build context for the parent — should have children populated.
        let parent_ctx = orch.build_context(parent_id).unwrap();
        assert_eq!(parent_ctx.children.len(), 1);
        assert_eq!(parent_ctx.children[0].goal, "child goal");
        assert!(matches!(
            parent_ctx.children[0].status,
            ChildStatus::Completed
        ));
    }

    /// `build_context` maps all `TaskPhase` variants to correct `ChildStatus`.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn build_context_child_status_mapping_all_phases() {
        let mut state = EpicState::new();
        let parent_id = state.next_task_id(); // T0
        let completed_id = state.next_task_id(); // T1
        let failed_id = state.next_task_id(); // T2
        let pending_id = state.next_task_id(); // T3
        let executing_id = state.next_task_id(); // T4
        let assessing_id = state.next_task_id(); // T5
        let verifying_id = state.next_task_id(); // T6

        let mut parent = Task::new(parent_id, None, "parent".into(), vec!["passes".into()], 0);
        parent.subtask_ids = vec![
            completed_id,
            failed_id,
            pending_id,
            executing_id,
            assessing_id,
            verifying_id,
        ];

        let mut completed_child = Task::new(
            completed_id,
            Some(parent_id),
            "completed child".into(),
            vec!["done".into()],
            1,
        );
        completed_child.phase = TaskPhase::Completed;

        let mut failed_child = Task::new(
            failed_id,
            Some(parent_id),
            "failed child".into(),
            vec!["done".into()],
            1,
        );
        failed_child.phase = TaskPhase::Failed;
        failed_child.attempts.push(Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("compile error".into()),
        });

        let pending_child = Task::new(
            pending_id,
            Some(parent_id),
            "pending child".into(),
            vec!["done".into()],
            1,
        );
        // pending_child.phase is Pending by default.

        let mut executing_child = Task::new(
            executing_id,
            Some(parent_id),
            "executing child".into(),
            vec!["done".into()],
            1,
        );
        executing_child.phase = TaskPhase::Executing;

        let mut assessing_child = Task::new(
            assessing_id,
            Some(parent_id),
            "assessing child".into(),
            vec!["done".into()],
            1,
        );
        assessing_child.phase = TaskPhase::Assessing;

        let mut verifying_child = Task::new(
            verifying_id,
            Some(parent_id),
            "verifying child".into(),
            vec!["done".into()],
            1,
        );
        verifying_child.phase = TaskPhase::Verifying;

        state.insert(parent);
        state.insert(completed_child);
        state.insert(failed_child);
        state.insert(pending_child);
        state.insert(executing_child);
        state.insert(assessing_child);
        state.insert(verifying_child);

        let mock = MockAgentService::new();
        let (tx, _rx) = events::event_channel();
        let orch = Orchestrator::new(mock, state, tx);

        let ctx = orch.build_context(parent_id).unwrap();
        assert_eq!(ctx.children.len(), 6);

        assert!(
            matches!(ctx.children[0].status, ChildStatus::Completed),
            "Completed phase should map to ChildStatus::Completed"
        );
        match &ctx.children[1].status {
            ChildStatus::Failed { reason } => {
                assert_eq!(reason, "compile error");
            }
            other => panic!("Failed phase should map to ChildStatus::Failed, got {other:?}"),
        }
        assert!(
            matches!(ctx.children[2].status, ChildStatus::Pending),
            "Pending phase should map to ChildStatus::Pending"
        );
        assert!(
            matches!(ctx.children[3].status, ChildStatus::InProgress),
            "Executing phase should map to ChildStatus::InProgress"
        );
        assert!(
            matches!(ctx.children[4].status, ChildStatus::InProgress),
            "Assessing phase should map to ChildStatus::InProgress"
        );
        assert!(
            matches!(ctx.children[5].status, ChildStatus::InProgress),
            "Verifying phase should map to ChildStatus::InProgress"
        );
    }

    /// `build_context` silently skips subtask IDs that don't exist in state.
    #[test]
    fn build_context_skips_dangling_subtask_id() {
        let mut state = EpicState::new();
        let parent_id = state.next_task_id(); // T0
        let real_child_id = state.next_task_id(); // T1
        let dangling_id = state.next_task_id(); // T2 — never inserted

        let mut parent = Task::new(parent_id, None, "parent".into(), vec!["passes".into()], 0);
        parent.subtask_ids = vec![real_child_id, dangling_id];

        let real_child = Task::new(
            real_child_id,
            Some(parent_id),
            "real child".into(),
            vec!["child passes".into()],
            1,
        );

        state.insert(parent);
        state.insert(real_child);
        // dangling_id is NOT inserted

        let mock = MockAgentService::new();
        let (tx, _rx) = events::event_channel();
        let orch = Orchestrator::new(mock, state, tx);

        let ctx = orch.build_context(parent_id).unwrap();
        assert_eq!(ctx.children.len(), 1, "should skip dangling subtask ID");
        assert_eq!(ctx.children[0].goal, "real child");
    }

    // -----------------------------------------------------------------------
    // File-level review tests
    // -----------------------------------------------------------------------

    /// Leaf passes file-level review -> completes normally.
    #[tokio::test]
    async fn file_level_review_pass_completes() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // File-level review passes.
        mock.file_level_review_responses
            .lock()
            .unwrap()
            .push_back(pass_file_level_review());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        assert_eq!(
            orch.state.get(child_id).unwrap().phase,
            TaskPhase::Completed
        );

        // FileLevelReviewCompleted event emitted with passed=true.
        let mut saw_review_passed = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::FileLevelReviewCompleted { task_id, passed } if task_id == child_id && passed)
            {
                saw_review_passed = true;
            }
        }
        assert!(
            saw_review_passed,
            "FileLevelReviewCompleted(passed=true) event not found"
        );
    }

    /// Leaf fails file-level review -> enters fix loop.
    #[tokio::test]
    async fn file_level_review_fail_triggers_fix_loop() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // File-level review fails.
        mock.file_level_review_responses
            .lock()
            .unwrap()
            .push_back(fail_file_level_review("missing error handling"));

        // Fix attempt succeeds.
        mock.fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // File-level review passes on second try.
        mock.file_level_review_responses
            .lock()
            .unwrap()
            .push_back(pass_file_level_review());

        // Root verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        assert_eq!(child.fix_attempts.len(), 1);

        // Both review events emitted: failed then passed.
        let mut review_events: Vec<bool> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let Event::FileLevelReviewCompleted { task_id, passed } = event {
                if task_id == child_id {
                    review_events.push(passed);
                }
            }
        }
        assert_eq!(
            review_events,
            vec![false, true],
            "expected [failed, passed] review events"
        );
    }

    /// Branch tasks skip file-level review, completing directly after verification.
    #[tokio::test]
    async fn branch_skips_file_level_review() {
        let mock = MockAgentService::new();

        mock.decompose_responses
            .lock()
            .unwrap()
            .push_back(one_subtask_decomposition());

        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Child verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Root (branch) verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // No file_review_responses queued for root — branch tasks skip review.
        // If branch tried to call file_level_review, the default mock would
        // pass; but we verify no event is emitted for the root.

        queue_file_level_reviews(&mock, 2);
        let (mut orch, root_id, mut rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // No FileLevelReviewCompleted event for the root (branch) task.
        let mut root_review_events = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, Event::FileLevelReviewCompleted { task_id, .. } if task_id == root_id)
            {
                root_review_events += 1;
            }
        }
        assert_eq!(
            root_review_events, 0,
            "branch tasks should not emit FileLevelReviewCompleted"
        );
    }

    /// Fix task (`is_fix_task=true`) that fails file-level review -> fails immediately (no fix loop).
    #[tokio::test]
    async fn fix_task_file_review_fail_no_fix_loop() {
        let mock = MockAgentService::new();

        // Root branches, original child succeeds, root verify fails.
        setup_branch_with_failing_root_verify(&mock, "root check failed");

        // Original child consumes file review from queue before fix subtask.
        mock.file_level_review_responses
            .lock()
            .unwrap()
            .push_back(pass_file_level_review());

        // Branch fix round 1: design returns 1 fix subtask.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());

        // Fix subtask: assessed as leaf.
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());

        // Fix subtask executes successfully.
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());

        // Fix subtask verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        // Fix subtask file-level review FAILS -> must fail immediately (is_fix_task).
        mock.file_level_review_responses
            .lock()
            .unwrap()
            .push_back(fail_file_level_review("fix incomplete"));

        // Root re-verification after round 1 (fix subtask failed).
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(fail_verification("root still failing"));

        // Round 2: fix subtask succeeds fully.
        mock.fix_subtask_responses
            .lock()
            .unwrap()
            .push_back(one_fix_subtask_decomposition());
        mock.assess_responses
            .lock()
            .unwrap()
            .push_back(leaf_assessment());
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(leaf_success());
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());
        mock.file_level_review_responses
            .lock()
            .unwrap()
            .push_back(pass_file_level_review());

        // Root re-verification passes.
        mock.verify_responses
            .lock()
            .unwrap()
            .push_back(pass_verification());

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        assert_eq!(root.verification_fix_rounds, 2);

        // Fix subtask from round 1 should have failed (no fix loop entered).
        let fix1_id = root.subtask_ids[1];
        let fix1 = orch.state.get(fix1_id).unwrap();
        assert!(fix1.is_fix_task);
        assert_eq!(fix1.phase, TaskPhase::Failed);
        assert_eq!(
            fix1.fix_attempts.len(),
            0,
            "fix task should not enter fix loop on file-level review failure"
        );
    }
}
