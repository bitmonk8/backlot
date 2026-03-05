// Recursive task execution, DFS traversal, state persistence, resume.

use crate::agent::{AgentService, SiblingSummary, TaskContext};
use crate::events::{Event, EventSender};
use crate::state::EpicState;
use crate::task::assess::AssessmentResult;
use crate::task::verify::VerificationOutcome;
use crate::task::branch::SubtaskSpec;
use crate::task::{Attempt, LeafResult, Magnitude, Model, Task, TaskId, TaskOutcome, TaskPath, TaskPhase};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use thiserror::Error;

const MAX_DEPTH: u32 = 8;
const RETRIES_PER_TIER: u32 = 3;
const MAX_BRANCH_FIX_ROUNDS: u32 = 3;
const MAX_ROOT_FIX_ROUNDS: u32 = 4;
const MAX_RECOVERY_ROUNDS: u32 = 2;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("task not found: {0}")]
    TaskNotFound(TaskId),
    #[error("agent error: {0}")]
    Agent(#[from] anyhow::Error),
}

#[derive(Debug, PartialEq, Eq)]
enum ScopeCheck {
    WithinBounds,
    Exceeded { metric: String, actual: u64, limit: u64 },
}

fn evaluate_scope(numstat_output: &str, magnitude: &Magnitude) -> ScopeCheck {
    let mut total_added: u64 = 0;
    let mut total_deleted: u64 = 0;
    let mut total_modified: u64 = 0;

    for line in numstat_output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        // Binary files show "-" for counts; skip them.
        let Ok(added) = parts[0].parse::<u64>() else { continue };
        let Ok(deleted) = parts[1].parse::<u64>() else { continue };
        let modified = added.min(deleted);
        total_added += added - modified;
        total_deleted += deleted - modified;
        total_modified += modified;
    }

    let multiplier = 3;
    if total_added > magnitude.max_lines_added * multiplier {
        return ScopeCheck::Exceeded {
            metric: "lines_added".into(),
            actual: total_added,
            limit: magnitude.max_lines_added * multiplier,
        };
    }
    if total_modified > magnitude.max_lines_modified * multiplier {
        return ScopeCheck::Exceeded {
            metric: "lines_modified".into(),
            actual: total_modified,
            limit: magnitude.max_lines_modified * multiplier,
        };
    }
    if total_deleted > magnitude.max_lines_deleted * multiplier {
        return ScopeCheck::Exceeded {
            metric: "lines_deleted".into(),
            actual: total_deleted,
            limit: magnitude.max_lines_deleted * multiplier,
        };
    }

    ScopeCheck::WithinBounds
}

pub struct Orchestrator<A: AgentService> {
    agent: A,
    state: EpicState,
    events: EventSender,
    state_path: Option<PathBuf>,
    project_root: Option<PathBuf>,
}

impl<A: AgentService> Orchestrator<A> {
    pub const fn new(agent: A, state: EpicState, events: EventSender) -> Self {
        Self {
            agent,
            state,
            events,
            state_path: None,
            project_root: None,
        }
    }

    pub fn with_state_path(mut self, path: PathBuf) -> Self {
        self.state_path = Some(path);
        self
    }

    pub fn with_project_root(mut self, path: PathBuf) -> Self {
        self.project_root = Some(path);
        self
    }

    pub async fn run(&mut self, root_id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
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
        if let Some(ref path) = self.state_path {
            if let Err(e) = self.state.save(path) {
                eprintln!("warning: state checkpoint failed: {e}");
            }
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
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

    fn complete_task_verified(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        self.transition(id, TaskPhase::Completed)?;
        self.emit(Event::VerificationComplete {
            task_id: id,
            passed: true,
        });
        self.emit(Event::TaskCompleted {
            task_id: id,
            outcome: TaskOutcome::Success,
        });
        self.checkpoint_save();
        Ok(TaskOutcome::Success)
    }

    fn create_subtasks(
        &mut self,
        parent_id: TaskId,
        specs: Vec<SubtaskSpec>,
        mark_fix: bool,
        append: bool,
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
        let task = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .clone();

        let parent_goal = task
            .parent_id
            .and_then(|pid| self.state.get(pid))
            .map(|p| p.goal.clone());

        let mut ancestor_goals = Vec::new();
        let mut cursor = task.parent_id;
        while let Some(pid) = cursor {
            if let Some(parent) = self.state.get(pid) {
                ancestor_goals.push(parent.goal.clone());
                cursor = parent.parent_id;
            } else {
                break;
            }
        }

        let (completed_siblings, pending_sibling_goals) = task
            .parent_id
            .and_then(|pid| self.state.get(pid))
            .map_or_else(
                || (Vec::new(), Vec::new()),
                |parent| {
                    let mut completed = Vec::new();
                    let mut pending = Vec::new();
                    for &sib_id in &parent.subtask_ids {
                        if sib_id == id {
                            continue;
                        }
                        let Some(sib) = self.state.get(sib_id) else {
                            continue;
                        };
                        match sib.phase {
                            TaskPhase::Completed => {
                                completed.push(SiblingSummary {
                                    id: sib_id,
                                    goal: sib.goal.clone(),
                                    outcome: TaskOutcome::Success,
                                    discoveries: sib.discoveries.clone(),
                                });
                            }
                            TaskPhase::Failed => {
                                let reason = sib
                                    .attempts
                                    .iter()
                                    .rev()
                                    .find_map(|a| a.error.clone())
                                    .unwrap_or_else(|| "unknown".into());
                                completed.push(SiblingSummary {
                                    id: sib_id,
                                    goal: sib.goal.clone(),
                                    outcome: TaskOutcome::Failed { reason },
                                    discoveries: sib.discoveries.clone(),
                                });
                            }
                            _ => {
                                pending.push(sib.goal.clone());
                            }
                        }
                    }
                    (completed, pending)
                },
            );

        Ok(TaskContext {
            task,
            parent_goal,
            ancestor_goals,
            completed_siblings,
            pending_sibling_goals,
        })
    }

    async fn check_scope_circuit_breaker(
        &self,
        task_id: TaskId,
    ) -> Result<ScopeCheck, OrchestratorError> {
        let task = self
            .state
            .get(task_id)
            .ok_or(OrchestratorError::TaskNotFound(task_id))?;

        let magnitude = match &task.magnitude {
            Some(m) => m.clone(),
            None => return Ok(ScopeCheck::WithinBounds),
        };

        let project_root = match &self.project_root {
            Some(p) => p.clone(),
            None => return Ok(ScopeCheck::WithinBounds),
        };

        let output = match tokio::process::Command::new("git")
            .args(["diff", "--numstat", "HEAD"])
            .current_dir(&project_root)
            .output()
            .await
        {
            Ok(o) if o.status.success() => o,
            _ => return Ok(ScopeCheck::WithinBounds),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(evaluate_scope(&stdout, &magnitude))
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

            // Resume: task was mid-verification. Re-verify without re-executing.
            {
                let task = self
                    .state
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                if task.path.is_some() && task.phase == TaskPhase::Verifying {
                    return self.finalize_task(id, TaskOutcome::Success).await;
                }
            }

            // Resume: task was mid-execution with path already set. Skip assessment.
            {
                let task = self
                    .state
                    .get(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                if task.path.is_some() && task.phase == TaskPhase::Executing {
                    let path = task.path.clone().unwrap();
                    self.transition(id, TaskPhase::Executing)?;
                    let outcome = match path {
                        TaskPath::Leaf => self.execute_leaf(id).await?,
                        TaskPath::Branch => self.execute_branch(id).await?,
                    };
                    return self.finalize_task(id, outcome).await;
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
            } else if depth >= MAX_DEPTH {
                AssessmentResult {
                    path: TaskPath::Leaf,
                    model: Model::Sonnet,
                    rationale: "Depth cap reached, forced to leaf".into(),
                    magnitude: None,
                }
            } else {
                let ctx = self.build_context(id)?;
                self.agent.assess(&ctx).await?
            };

            // Apply assessment to task.
            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.path = Some(assessment.path.clone());
                task.model = Some(assessment.model);
                task.current_model = Some(assessment.model);
                task.magnitude.clone_from(&assessment.magnitude);
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

            let outcome = match assessment.path {
                TaskPath::Leaf => self.execute_leaf(id).await?,
                TaskPath::Branch => self.execute_branch(id).await?,
            };

            self.finalize_task(id, outcome).await
        })
    }

    async fn finalize_task(
        &mut self,
        id: TaskId,
        outcome: TaskOutcome,
    ) -> Result<TaskOutcome, OrchestratorError> {
        if outcome == TaskOutcome::Success {
            self.transition(id, TaskPhase::Verifying)?;
            self.emit(Event::VerificationStarted { task_id: id });

            let ctx = self.build_context(id)?;
            let verify_result = self.agent.verify(&ctx).await?;

            match verify_result.outcome {
                VerificationOutcome::Pass => self.complete_task_verified(id),
                VerificationOutcome::Fail { reason } => {
                    self.emit(Event::VerificationComplete {
                        task_id: id,
                        passed: false,
                    });

                    let task = self.state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
                    let is_leaf = task.path == Some(TaskPath::Leaf);

                    if is_leaf {
                        self.leaf_fix_loop(id, &reason).await
                    } else {
                        let task = self.state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
                        let is_fix_task = task.is_fix_task;

                        if is_fix_task {
                            // Fix subtasks cannot trigger branch fix loop (prevents recursive fix chains).
                            self.fail_task(id, reason)
                        } else {
                            self.branch_fix_loop(id, &reason).await
                        }
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

    #[allow(clippy::too_many_lines)] // Single loop with distinct phases; splitting adds indirection.
    async fn leaf_fix_loop(
        &mut self,
        id: TaskId,
        initial_failure: &str,
    ) -> Result<TaskOutcome, OrchestratorError> {
        // Start fix attempts at the model that produced the failing output.
        let mut fix_model = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .current_model
            .unwrap_or(Model::Haiku);

        // Resume-safe: count consecutive trailing attempts at the current tier
        // so we don't grant extra retries after a crash mid-fix-loop.
        #[allow(clippy::cast_possible_truncation)]
        let mut fix_retries_at_tier: u32 = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .fix_attempts
            .iter()
            .rev()
            .take_while(|a| a.model == fix_model)
            .count() as u32;
        let mut failure_reason = initial_failure.to_owned();

        loop {
            // Scope circuit breaker check.
            match self.check_scope_circuit_breaker(id).await? {
                ScopeCheck::WithinBounds => {}
                ScopeCheck::Exceeded { metric, actual, limit } => {
                    return self.fail_task(
                        id,
                        format!("SCOPE_EXCEEDED: {metric} actual={actual} limit={limit}"),
                    );
                }
            }

            #[allow(clippy::cast_possible_truncation)] // Fix attempts capped at 9 (3 tiers × 3 retries).
            let attempt_number = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .fix_attempts
                .len() as u32
                + 1;

            self.emit(Event::FixAttempt {
                task_id: id,
                attempt: attempt_number,
                model: fix_model,
            });

            let ctx = self.build_context(id)?;
            let LeafResult { outcome, discoveries } = self
                .agent
                .fix_leaf(&ctx, fix_model, &failure_reason, attempt_number)
                .await?;

            // Record attempt and discoveries.
            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.fix_attempts.push(Attempt {
                    model: fix_model,
                    succeeded: outcome == TaskOutcome::Success,
                    error: match &outcome {
                        TaskOutcome::Success => None,
                        TaskOutcome::Failed { reason } => Some(reason.clone()),
                    },
                });
                if !discoveries.is_empty() {
                    let count = discoveries.len();
                    task.discoveries.extend(discoveries);
                    self.emit(Event::DiscoveriesRecorded {
                        task_id: id,
                        count,
                    });
                }
            }

            // Persist fix attempt before verification so it survives a crash.
            self.checkpoint_save();

            if outcome == TaskOutcome::Success {
                // Re-verify after successful fix.
                self.emit(Event::VerificationStarted { task_id: id });
                let ctx = self.build_context(id)?;
                let verify_result = self.agent.verify(&ctx).await?;

                match verify_result.outcome {
                    VerificationOutcome::Pass => {
                        return self.complete_task_verified(id);
                    }
                    VerificationOutcome::Fail { reason } => {
                        self.emit(Event::VerificationComplete {
                            task_id: id,
                            passed: false,
                        });
                        failure_reason = reason;
                    }
                }
            } else if let TaskOutcome::Failed { reason } = &outcome {
                failure_reason = reason.clone();
            }

            self.checkpoint_save();

            fix_retries_at_tier += 1;

            if fix_retries_at_tier < RETRIES_PER_TIER {
                continue;
            }

            // Escalate model tier.
            if let Some(next_model) = fix_model.escalate() {
                self.emit(Event::FixModelEscalated {
                    task_id: id,
                    from: fix_model,
                    to: next_model,
                });
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.current_model = Some(next_model);
                fix_model = next_model;
                fix_retries_at_tier = 0;
                continue;
            }

            // All tiers exhausted — terminal failure.
            return self.fail_task(id, failure_reason);
        }
    }

    #[allow(clippy::too_many_lines)] // Single loop with distinct phases; splitting adds indirection.
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

        let max_rounds = if is_root { MAX_ROOT_FIX_ROUNDS } else { MAX_BRANCH_FIX_ROUNDS };
        let mut failure_reason = initial_failure.to_owned();

        loop {
            // Check round budget before starting a new round.
            let current_rounds = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .verification_fix_rounds;

            if current_rounds >= max_rounds {
                return self.fail_task(id, failure_reason);
            }

            let round = {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.verification_fix_rounds += 1;
                task.verification_fix_rounds
            };

            // Sonnet for rounds 1-3, Opus for round 4 (root only).
            let model = if round <= 3 { Model::Sonnet } else { Model::Opus };

            match self.check_scope_circuit_breaker(id).await? {
                ScopeCheck::WithinBounds => {}
                ScopeCheck::Exceeded { metric, actual, limit } => {
                    return self.fail_task(
                        id,
                        format!("SCOPE_EXCEEDED: {metric} actual={actual} limit={limit}"),
                    );
                }
            }

            self.emit(Event::BranchFixRound {
                task_id: id,
                round,
                model,
            });

            let ctx = self.build_context(id)?;
            let decomposition = self
                .agent
                .design_fix_subtasks(&ctx, model, &failure_reason, round)
                .await?;

            if decomposition.subtasks.is_empty() {
                "fix agent produced no subtasks".clone_into(&mut failure_reason);
                self.checkpoint_save();
                continue;
            }

            let fix_child_ids = self.create_subtasks(id, decomposition.subtasks, true, true)?;
            self.emit(Event::FixSubtasksCreated {
                task_id: id,
                count: fix_child_ids.len(),
                round,
            });

            // Execute each fix subtask. Task-level failures (Ok(Failed)) are tolerated;
            // infrastructure errors (Err) propagate and abort the run.
            for &child_id in &fix_child_ids {
                let _child_outcome = self.execute_task(child_id).await?;
            }

            // Re-verify the branch after fix subtasks complete.
            self.emit(Event::VerificationStarted { task_id: id });
            let ctx = self.build_context(id)?;
            let verify_result = self.agent.verify(&ctx).await?;

            match verify_result.outcome {
                VerificationOutcome::Pass => {
                    return self.complete_task_verified(id);
                }
                VerificationOutcome::Fail { reason } => {
                    self.emit(Event::VerificationComplete {
                        task_id: id,
                        passed: false,
                    });
                    failure_reason = reason;
                }
            }

            self.checkpoint_save();
        }
    }

    async fn execute_leaf(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        let mut retries_at_tier: u32 = 0;

        loop {
            let current_model = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .current_model
                .unwrap_or(Model::Haiku);

            let ctx = self.build_context(id)?;
            let LeafResult {
                outcome,
                discoveries,
            } = self.agent.execute_leaf(&ctx, current_model).await?;

            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.attempts.push(Attempt {
                    model: current_model,
                    succeeded: outcome == TaskOutcome::Success,
                    error: match &outcome {
                        TaskOutcome::Success => None,
                        TaskOutcome::Failed { reason } => Some(reason.clone()),
                    },
                });
                // Discoveries accumulate across retries without deduplication.
                // LLM-generated text is unlikely to repeat verbatim; if this
                // becomes noisy, add a Haiku dedup pass before storing.
                if !discoveries.is_empty() {
                    let count = discoveries.len();
                    task.discoveries.extend(discoveries);
                    self.emit(Event::DiscoveriesRecorded {
                        task_id: id,
                        count,
                    });
                }
            }

            if outcome == TaskOutcome::Success {
                return Ok(outcome);
            }

            retries_at_tier += 1;

            if retries_at_tier < RETRIES_PER_TIER {
                self.emit(Event::RetryAttempt {
                    task_id: id,
                    attempt: retries_at_tier,
                    model: current_model,
                });
                continue;
            }

            if let Some(next_model) = current_model.escalate() {
                self.emit(Event::ModelEscalated {
                    task_id: id,
                    from: current_model,
                    to: next_model,
                });
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.current_model = Some(next_model);
                retries_at_tier = 0;
                continue;
            }

            // All tiers exhausted — terminal failure.
            return Ok(outcome);
        }
    }

    async fn execute_branch(&mut self, id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        // Resume: reuse existing subtasks if already decomposed.
        let existing_subtasks = self
            .state
            .get(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?
            .subtask_ids
            .clone();

        if existing_subtasks.is_empty() {
            let ctx = self.build_context(id)?;
            let decomposition = self.agent.design_and_decompose(&ctx).await?;

            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.decomposition_rationale = Some(decomposition.rationale);
            }

            let new_child_ids = self.create_subtasks(id, decomposition.subtasks, false, false)?;
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
                    let ctx = self.build_context(id)?;
                    let _decision = self.agent.checkpoint(&ctx, &child_discoveries).await?;
                    // Checkpoint adjust/escalate deferred; decision treated as Proceed.
                }

                if let TaskOutcome::Failed { ref reason } = child_outcome {
                    if let Some(recovery_outcome) =
                        self.attempt_recovery(id, reason).await?
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

        // If the loop exits normally, all unrecovered failures were already
        // propagated via attempt_recovery. Remaining Failed children are those
        // whose failures were handled by recovery subtasks.
        Ok(TaskOutcome::Success)
    }

    /// Attempt recovery after a child failure. Returns `Some(Failed)` if recovery
    /// is not possible or rounds are exhausted. Returns `None` if recovery subtasks
    /// were created successfully (caller should restart the child loop).
    #[allow(clippy::too_many_lines)] // Linear sequence of recovery steps; splitting adds indirection.
    async fn attempt_recovery(
        &mut self,
        parent_id: TaskId,
        failure_reason: &str,
    ) -> Result<Option<TaskOutcome>, OrchestratorError> {
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

        // Check recovery round budget.
        if task.recovery_rounds >= MAX_RECOVERY_ROUNDS {
            return Ok(Some(TaskOutcome::Failed {
                reason: format!(
                    "recovery rounds exhausted ({MAX_RECOVERY_ROUNDS}): {failure_reason}"
                ),
            }));
        }

        let round = task.recovery_rounds + 1;

        // Step 1: assess whether recovery is possible.
        // Agent errors treated as unrecoverable (avoids aborting the entire run
        // due to transient errors like rate limits or malformed responses).
        let ctx = self.build_context(parent_id)?;
        let strategy = match self.agent.assess_recovery(&ctx, failure_reason).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Ok(Some(TaskOutcome::Failed {
                    reason: failure_reason.to_string(),
                }));
            }
            Err(e) => {
                eprintln!("warning: recovery assessment failed: {e}");
                return Ok(Some(TaskOutcome::Failed {
                    reason: failure_reason.to_string(),
                }));
            }
        };

        self.emit(Event::RecoveryStarted {
            task_id: parent_id,
            round,
        });

        // Increment recovery round counter before subtask creation so that a
        // crash between subtask creation and counter update does not grant an
        // extra recovery round on resume.
        {
            let task = self
                .state
                .get_mut(parent_id)
                .ok_or(OrchestratorError::TaskNotFound(parent_id))?;
            task.recovery_rounds = round;
        }
        self.checkpoint_save();

        // Step 2: design recovery subtasks (Opus).
        // Agent errors treated as failed recovery round (round already consumed).
        let ctx = self.build_context(parent_id)?;
        let plan = match self
            .agent
            .design_recovery_subtasks(&ctx, failure_reason, &strategy, round)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("warning: recovery plan design failed: {e}");
                return Ok(Some(TaskOutcome::Failed {
                    reason: format!("recovery design failed: {failure_reason}"),
                }));
            }
        };

        if plan.subtasks.is_empty() {
            return Ok(Some(TaskOutcome::Failed {
                reason: format!("recovery produced no subtasks: {failure_reason}"),
            }));
        }

        let approach = if plan.full_redecomposition {
            "full"
        } else {
            "incremental"
        };
        self.emit(Event::RecoveryPlanSelected {
            task_id: parent_id,
            approach: approach.into(),
        });

        // For full re-decomposition, mark remaining pending children as Failed.
        if plan.full_redecomposition {
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

        // Step 3: create recovery subtasks (appended to parent's subtask_ids).
        // Recovery subtasks are NOT marked is_fix_task: they are full tasks that
        // get their own assessment, verification, fix loops, and recovery budget.
        // Recursion is bounded by MAX_DEPTH (8) and each level's recovery budget
        // (MAX_RECOVERY_ROUNDS=2).
        let count = plan.subtasks.len();
        let recovery_child_ids =
            self.create_subtasks(parent_id, plan.subtasks, false, true)?;
        self.emit(Event::RecoverySubtasksCreated {
            task_id: parent_id,
            count,
            round,
        });
        self.emit(Event::SubtasksCreated {
            parent_id,
            child_ids: recovery_child_ids,
        });

        // Return None to signal caller should restart the child loop.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{self, EventReceiver};
    use crate::task::assess::AssessmentResult;
    use crate::task::branch::{CheckpointDecision, DecompositionResult, SubtaskSpec};
    use crate::task::verify::{VerificationOutcome, VerificationResult};
    use crate::task::{LeafResult, Magnitude, MagnitudeEstimate, RecoveryPlan, TaskPath};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[allow(clippy::struct_field_names)]
    struct MockAgentService {
        assess_responses: Mutex<VecDeque<AssessmentResult>>,
        leaf_responses: Mutex<VecDeque<LeafResult>>,
        fix_leaf_responses: Mutex<VecDeque<LeafResult>>,
        decompose_responses: Mutex<VecDeque<DecompositionResult>>,
        fix_subtask_responses: Mutex<VecDeque<DecompositionResult>>,
        verify_responses: Mutex<VecDeque<VerificationResult>>,
        checkpoint_responses: Mutex<VecDeque<CheckpointDecision>>,
        recovery_responses: Mutex<VecDeque<Option<String>>>,
        recovery_plan_responses: Mutex<VecDeque<RecoveryPlan>>,
    }

    impl MockAgentService {
        fn new() -> Self {
            Self {
                assess_responses: Mutex::new(VecDeque::new()),
                leaf_responses: Mutex::new(VecDeque::new()),
                fix_leaf_responses: Mutex::new(VecDeque::new()),
                decompose_responses: Mutex::new(VecDeque::new()),
                fix_subtask_responses: Mutex::new(VecDeque::new()),
                verify_responses: Mutex::new(VecDeque::new()),
                checkpoint_responses: Mutex::new(VecDeque::new()),
                recovery_responses: Mutex::new(VecDeque::new()),
                recovery_plan_responses: Mutex::new(VecDeque::new()),
            }
        }
    }

    impl AgentService for MockAgentService {
        async fn assess(&self, _ctx: &TaskContext) -> anyhow::Result<AssessmentResult> {
            Ok(self
                .assess_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no assess response queued"))
        }

        async fn execute_leaf(
            &self,
            _ctx: &TaskContext,
            _model: Model,
        ) -> anyhow::Result<LeafResult> {
            Ok(self
                .leaf_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no leaf response queued"))
        }

        async fn fix_leaf(
            &self,
            _ctx: &TaskContext,
            _model: Model,
            _failure_reason: &str,
            _attempt: u32,
        ) -> anyhow::Result<LeafResult> {
            Ok(self
                .fix_leaf_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no fix_leaf response queued"))
        }

        async fn design_and_decompose(
            &self,
            _ctx: &TaskContext,
        ) -> anyhow::Result<DecompositionResult> {
            Ok(self
                .decompose_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no decompose response queued"))
        }

        async fn design_fix_subtasks(
            &self,
            _ctx: &TaskContext,
            _model: Model,
            _verification_issues: &str,
            _round: u32,
        ) -> anyhow::Result<DecompositionResult> {
            Ok(self
                .fix_subtask_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no fix_subtask response queued"))
        }

        async fn verify(&self, _ctx: &TaskContext) -> anyhow::Result<VerificationResult> {
            Ok(self
                .verify_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no verify response queued"))
        }

        async fn checkpoint(
            &self,
            _ctx: &TaskContext,
            _discoveries: &[String],
        ) -> anyhow::Result<CheckpointDecision> {
            Ok(self
                .checkpoint_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no checkpoint response queued"))
        }

        async fn assess_recovery(
            &self,
            _ctx: &TaskContext,
            _failure_reason: &str,
        ) -> anyhow::Result<Option<String>> {
            Ok(self
                .recovery_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no recovery response queued"))
        }

        async fn design_recovery_subtasks(
            &self,
            _ctx: &TaskContext,
            _failure_reason: &str,
            _strategy: &str,
            _recovery_round: u32,
        ) -> anyhow::Result<RecoveryPlan> {
            Ok(self
                .recovery_plan_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no recovery_plan response queued"))
        }
    }

    fn pass_verification() -> VerificationResult {
        VerificationResult {
            outcome: VerificationOutcome::Pass,
            details: "all checks passed".into(),
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
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(None);

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

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        orch.state.set_root_id(root_id);
        orch.state_path = Some(state_path.clone());

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
        root.model = Some(Model::Sonnet);
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

    /// Resume: existing subtask_ids on root skips decomposition.
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
        root.model = Some(Model::Sonnet);
        root.current_model = Some(Model::Sonnet);
        root.phase = TaskPhase::Executing;
        root.subtask_ids = vec![mid_id];

        // Mid: Executing, Branch, has grandchild. Was mid-execution when killed.
        let mut mid = Task::new(mid_id, Some(root_id), "mid".into(), vec!["mid passes".into()], 1);
        mid.path = Some(TaskPath::Branch);
        mid.model = Some(Model::Sonnet);
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
        root.model = Some(Model::Sonnet);
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
        child.model = Some(Model::Haiku);
        child.current_model = Some(Model::Haiku);
        child.phase = TaskPhase::Verifying;

        state.insert(root);
        state.insert(child);

        let (tx, _rx) = events::event_channel();
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        // Child went straight to verification, not re-execution.
        assert_eq!(orch.state.get(child_id).unwrap().phase, TaskPhase::Completed);
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

        // Set up state with root at depth MAX_DEPTH - 1 so child hits cap.
        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let root = Task::new(
            root_id,
            None,
            "deep root".into(),
            vec!["passes".into()],
            MAX_DEPTH - 1,
        );
        state.insert(root);
        let (tx, _rx) = events::event_channel();
        let mut orch = Orchestrator::new(mock, state, tx);

        // Root is not at depth 0 but has no parent, so it's forced to Branch.
        // Child will be at MAX_DEPTH, forced to Leaf (no assess call needed).
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child_id = orch.state.get(root_id).unwrap().subtask_ids[0];
        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.path, Some(TaskPath::Leaf));
        assert_eq!(child.depth, MAX_DEPTH);
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
        assert!(found_discoveries_event, "DiscoveriesRecorded event not found");
    }

    /// Task with magnitude set but no git repo → WithinBounds (best-effort).
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
        let result = orch.check_scope_circuit_breaker(task_id).await.unwrap();
        assert!(matches!(result, ScopeCheck::WithinBounds));
    }

    /// Task with no magnitude → WithinBounds (skip check).
    #[tokio::test]
    async fn scope_check_skipped_no_magnitude() {
        let mock = MockAgentService::new();
        let mut state = EpicState::new();
        let task_id = state.next_task_id();
        let task = Task::new(task_id, None, "test".into(), vec![], 0);
        state.insert(task);

        let (tx, _rx) = events::event_channel();
        let orch = Orchestrator::new(mock, state, tx);

        let result = orch.check_scope_circuit_breaker(task_id).await.unwrap();
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

    /// Leaf fix loop: verification fails → fix_leaf succeeds → re-verification passes.
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
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(None);

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
        mock.decompose_responses.lock().unwrap().push_back(
            DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "mid branch".into(),
                    verification_criteria: vec!["mid passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Medium,
                }],
                rationale: "one mid branch".into(),
            },
        );

        // Mid is assessed as branch.
        mock.assess_responses.lock().unwrap().push_back(
            AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            },
        );

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
        mock.recovery_responses
            .lock()
            .unwrap()
            .push_back(None);

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
        mock.fix_subtask_responses.lock().unwrap().push_back(
            DecompositionResult {
                subtasks: vec![SubtaskSpec {
                    goal: "complex fix".into(),
                    verification_criteria: vec!["fix passes".into()],
                    magnitude_estimate: MagnitudeEstimate::Medium,
                }],
                rationale: "complex fix needed".into(),
            },
        );

        // Fix subtask assessed as branch.
        mock.assess_responses.lock().unwrap().push_back(
            AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            },
        );

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

    // --- evaluate_scope pure function tests ---

    #[test]
    fn evaluate_scope_within_bounds() {
        let output = "10\t5\tfile1.rs\n3\t0\tfile2.rs";
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        // file1: added=10, deleted=5 → modified=min(10,5)=5 → net_added=5, net_deleted=0
        // file2: added=3, deleted=0 → modified=0 → net_added=3, net_deleted=0
        // totals: added=8, modified=5, deleted=0
        // 3x limits: added=30, modified=15, deleted=15
        // All within bounds.
        assert_eq!(evaluate_scope(output, &magnitude), ScopeCheck::WithinBounds);
    }

    #[test]
    fn evaluate_scope_exceeded() {
        let output = "100\t0\tfile1.rs";
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        // 100 added > 30 (3×10).
        let result = evaluate_scope(output, &magnitude);
        match result {
            ScopeCheck::Exceeded { metric, actual, limit } => {
                assert_eq!(metric, "lines_added");
                assert_eq!(actual, 100);
                assert_eq!(limit, 30);
            }
            ScopeCheck::WithinBounds => panic!("expected Exceeded"),
        }
    }

    #[test]
    fn evaluate_scope_binary_files_skipped() {
        let output = "-\t-\tbinary.png\n5\t2\tcode.rs";
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        // Binary line skipped. code.rs: added=5, deleted=2 → modified=2 → net_added=3, net_deleted=0.
        // All within 3x bounds.
        assert_eq!(evaluate_scope(output, &magnitude), ScopeCheck::WithinBounds);
    }

    #[test]
    fn evaluate_scope_empty_output() {
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        assert_eq!(evaluate_scope("", &magnitude), ScopeCheck::WithinBounds);
    }

    /// Resume mid-fix-loop: pre-existing fix_attempts are counted so retries_at_tier is correct.
    #[tokio::test]
    async fn leaf_fix_persists_and_resumes() {
        let mock = MockAgentService::new();

        // The child is already in Verifying with 2 fix_attempts at Haiku.
        // execute_task sees Verifying → finalize_task(Success) → verify → fail → leaf_fix_loop.
        // leaf_fix_loop initializes fix_retries_at_tier=2 from the 2 existing attempts.
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
        root.model = Some(Model::Sonnet);
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
        child.model = Some(Model::Haiku);
        child.current_model = Some(Model::Haiku);
        child.phase = TaskPhase::Verifying;
        child.fix_attempts = vec![
            Attempt { model: Model::Haiku, succeeded: false, error: Some("fail1".into()) },
            Attempt { model: Model::Haiku, succeeded: false, error: Some("fail2".into()) },
        ];

        state.insert(root);
        state.insert(child);

        let (tx, _rx) = events::event_channel();
        let mut orch = Orchestrator::new(mock, state, tx);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let child = orch.state.get(child_id).unwrap();
        assert_eq!(child.phase, TaskPhase::Completed);
        assert_eq!(child.fix_attempts.len(), 3); // 2 pre-existing + 1 new
        assert!(child.fix_attempts[2].succeeded);
        assert_eq!(child.fix_attempts[2].model, Model::Haiku);
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
            if let Event::BranchFixRound { task_id, round, model } = event {
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
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("A failed"));
        }

        // Recovery: assess says recoverable, plan is incremental.
        mock.recovery_responses.lock().unwrap().push_back(Some("retry differently".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(incremental_recovery_plan());

        // Recovery subtask: assessed as leaf, succeeds.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Child B (still pending, runs after recovery): assessed as leaf, succeeds.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        // Original 2 children + 1 recovery subtask = 3.
        assert_eq!(root.subtask_ids.len(), 3);
        assert_eq!(root.recovery_rounds, 1);

        // Child A should be Failed.
        let child_a_id = root.subtask_ids[0];
        assert_eq!(orch.state.get(child_a_id).unwrap().phase, TaskPhase::Failed);

        // Child B (pending sibling) should have completed after recovery.
        let child_b_id = root.subtask_ids[1];
        assert_eq!(orch.state.get(child_b_id).unwrap().phase, TaskPhase::Completed);

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
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("A failed"));
        }

        // Recovery: full re-decomposition.
        mock.recovery_responses.lock().unwrap().push_back(Some("redo everything".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(full_recovery_plan());

        // Recovery subtask: assessed as leaf, succeeds.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

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

        mock.decompose_responses.lock().unwrap().push_back(DecompositionResult {
            subtasks: vec![SubtaskSpec {
                goal: "child A".into(),
                verification_criteria: vec!["A passes".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            }],
            rationale: "one subtask".into(),
        });

        // Child A fails terminally.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("A failed"));
        }

        // Round 1: recovery with incremental plan.
        mock.recovery_responses.lock().unwrap().push_back(Some("try again".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(incremental_recovery_plan());

        // Recovery subtask 1: fails terminally.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("recovery 1 failed"));
        }

        // Round 2: another recovery attempt.
        mock.recovery_responses.lock().unwrap().push_back(Some("try again".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(incremental_recovery_plan());

        // Recovery subtask 2: also fails terminally.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("recovery 2 failed"));
        }

        // Round 3 would exceed limit — no more recovery responses needed.

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert!(matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted")));
        assert_eq!(orch.state.get(root_id).unwrap().recovery_rounds, 2);
    }

    /// Fix tasks do not attempt recovery (prevents recursive recovery chains).
    /// A fix task that is a branch with a failing child should propagate failure
    /// without calling assess_recovery.
    #[tokio::test]
    async fn recovery_not_attempted_for_fix_tasks() {
        // Directly test attempt_recovery by creating a task marked is_fix_task=true.
        let mock = MockAgentService::new();
        let mut state = EpicState::new();

        let root_id = state.next_task_id();
        let mut root = Task::new(
            root_id,
            None,
            "fix parent".into(),
            vec!["passes".into()],
            0,
        );
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

    /// assess_recovery returns None → child failure propagates immediately.
    #[tokio::test]
    async fn recovery_not_attempted_when_unrecoverable() {
        let mock = MockAgentService::new();

        mock.decompose_responses.lock().unwrap().push_back(one_subtask_decomposition());
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("terminal"));
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

        mock.decompose_responses.lock().unwrap().push_back(one_subtask_decomposition());
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("A failed"));
        }

        // Round 1 recovery.
        mock.recovery_responses.lock().unwrap().push_back(Some("try again".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(incremental_recovery_plan());

        // Recovery subtask succeeds.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

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

        mock.decompose_responses.lock().unwrap().push_back(one_subtask_decomposition());
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("broke"));
        }

        // Recovery: assess says recoverable, but plan has no subtasks.
        mock.recovery_responses.lock().unwrap().push_back(Some("try something".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(RecoveryPlan {
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
        mock.decompose_responses.lock().unwrap().push_back(DecompositionResult {
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
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Child B: fails terminally.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("B failed"));
        }

        // Recovery: full re-decomposition.
        mock.recovery_responses.lock().unwrap().push_back(Some("redo".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(full_recovery_plan());

        // Recovery subtask: succeeds.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Root verification passes.
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        let (mut orch, root_id, _rx) = make_orchestrator(mock);
        let result = orch.run(root_id).await.unwrap();
        assert_eq!(result, TaskOutcome::Success);

        let root = orch.state.get(root_id).unwrap();
        // A, B, C (original) + recovery = 4.
        assert_eq!(root.subtask_ids.len(), 4);

        // A: Completed (untouched by full re-decomposition).
        assert_eq!(orch.state.get(root.subtask_ids[0]).unwrap().phase, TaskPhase::Completed);
        // B: Failed (the one that triggered recovery).
        assert_eq!(orch.state.get(root.subtask_ids[1]).unwrap().phase, TaskPhase::Failed);
        // C: Failed (superseded — was Pending when full re-decomposition ran).
        assert_eq!(orch.state.get(root.subtask_ids[2]).unwrap().phase, TaskPhase::Failed);
        // Recovery subtask: Completed.
        assert_eq!(orch.state.get(root.subtask_ids[3]).unwrap().phase, TaskPhase::Completed);
    }

    /// Recovery events are emitted correctly.
    #[tokio::test]
    async fn recovery_emits_events() {
        let mock = MockAgentService::new();

        mock.decompose_responses.lock().unwrap().push_back(one_subtask_decomposition());
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        for _ in 0..9 {
            mock.leaf_responses.lock().unwrap().push_back(leaf_failed("broke"));
        }

        mock.recovery_responses.lock().unwrap().push_back(Some("fix it".into()));
        mock.recovery_plan_responses.lock().unwrap().push_back(incremental_recovery_plan());

        // Recovery subtask succeeds.
        mock.assess_responses.lock().unwrap().push_back(leaf_assessment());
        mock.leaf_responses.lock().unwrap().push_back(leaf_success());
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

        // Root verification.
        mock.verify_responses.lock().unwrap().push_back(pass_verification());

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
                Event::RecoveryPlanSelected { task_id, ref approach } => {
                    assert_eq!(task_id, root_id);
                    assert_eq!(approach, "incremental");
                    saw_recovery_plan = true;
                }
                Event::RecoverySubtasksCreated { task_id, count, round } => {
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
}
