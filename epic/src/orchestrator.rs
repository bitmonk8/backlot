// Recursive task execution, DFS traversal, state persistence, resume.

use crate::agent::{AgentService, SiblingSummary, TaskContext};
use crate::events::{Event, EventSender};
use crate::state::EpicState;
use crate::task::assess::AssessmentResult;
use crate::task::verify::VerificationOutcome;
use crate::task::{Attempt, Model, Task, TaskId, TaskOutcome, TaskPath, TaskPhase};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use thiserror::Error;

const MAX_DEPTH: u32 = 8;
const RETRIES_PER_TIER: u32 = 3;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("task not found: {0}")]
    TaskNotFound(TaskId),
    #[error("agent error: {0}")]
    Agent(#[from] anyhow::Error),
}

pub struct Orchestrator<A: AgentService> {
    agent: A,
    state: EpicState,
    events: EventSender,
    state_path: Option<PathBuf>,
}

impl<A: AgentService> Orchestrator<A> {
    pub const fn new(agent: A, state: EpicState, events: EventSender) -> Self {
        Self {
            agent,
            state,
            events,
            state_path: None,
        }
    }

    pub fn with_state_path(mut self, path: PathBuf) -> Self {
        self.state_path = Some(path);
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
                                completed.push(SiblingSummary {
                                    id: sib_id,
                                    goal: sib.goal.clone(),
                                    outcome: TaskOutcome::Failed {
                                        reason: "failed".into(),
                                    },
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
                }
            } else if depth >= MAX_DEPTH {
                AssessmentResult {
                    path: TaskPath::Leaf,
                    model: Model::Sonnet,
                    rationale: "Depth cap reached, forced to leaf".into(),
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
                VerificationOutcome::Pass => {
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
                VerificationOutcome::Fail { reason } => {
                    // Fix loop deferred for v1 — verification failure = task failure.
                    self.transition(id, TaskPhase::Failed)?;
                    self.emit(Event::VerificationComplete {
                        task_id: id,
                        passed: false,
                    });
                    let outcome = TaskOutcome::Failed { reason };
                    self.emit(Event::TaskCompleted {
                        task_id: id,
                        outcome: outcome.clone(),
                    });
                    self.checkpoint_save();
                    Ok(outcome)
                }
            }
        } else {
            self.transition(id, TaskPhase::Failed)?;
            self.emit(Event::TaskCompleted {
                task_id: id,
                outcome: outcome.clone(),
            });
            self.checkpoint_save();
            Ok(outcome)
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
            let outcome = self.agent.execute_leaf(&ctx, current_model).await?;

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

        let child_ids = if existing_subtasks.is_empty() {
            let ctx = self.build_context(id)?;
            let decomposition = self.agent.design_and_decompose(&ctx).await?;

            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.decomposition_rationale = Some(decomposition.rationale);
            }

            let parent_depth = self
                .state
                .get(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?
                .depth;

            let mut new_child_ids = Vec::new();
            for spec in decomposition.subtasks {
                let child_id = self.state.next_task_id();
                let mut child = Task::new(
                    child_id,
                    Some(id),
                    spec.goal,
                    spec.verification_criteria,
                    parent_depth + 1,
                );
                child.magnitude_estimate = Some(spec.magnitude_estimate);
                new_child_ids.push(child_id);
                self.state.insert(child);
            }

            {
                let task = self
                    .state
                    .get_mut(id)
                    .ok_or(OrchestratorError::TaskNotFound(id))?;
                task.subtask_ids.clone_from(&new_child_ids);
            }

            for &child_id in &new_child_ids {
                if let Some(child) = self.state.get(child_id) {
                    self.emit(Event::TaskRegistered {
                        task_id: child_id,
                        parent_id: child.parent_id,
                        goal: child.goal.clone(),
                        depth: child.depth,
                    });
                }
            }
            self.emit(Event::SubtasksCreated {
                parent_id: id,
                child_ids: new_child_ids.clone(),
            });
            self.checkpoint_save();

            new_child_ids
        } else {
            existing_subtasks
        };

        for &child_id in &child_ids {
            // Resume: skip children in terminal phases.
            let child_phase = self
                .state
                .get(child_id)
                .ok_or(OrchestratorError::TaskNotFound(child_id))?
                .phase;

            match child_phase {
                TaskPhase::Completed => continue,
                TaskPhase::Failed => {
                    let reason = "previously failed".to_string();
                    let ctx = self.build_context(id)?;
                    let _recovery = self.agent.assess_recovery(&ctx, &reason).await?;
                    return Ok(TaskOutcome::Failed { reason });
                }
                _ => {}
            }

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
                // Adjust and Escalate deferred for v1 — all treated as Proceed.
            }

            if let TaskOutcome::Failed { ref reason } = child_outcome {
                // Structurally present: call assess_recovery but don't act on result yet.
                // Full re-decomposition deferred for v1.
                let ctx = self.build_context(id)?;
                let _recovery = self.agent.assess_recovery(&ctx, reason).await?;
                return Ok(TaskOutcome::Failed {
                    reason: reason.clone(),
                });
            }
        }

        Ok(TaskOutcome::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{self, EventReceiver};
    use crate::task::assess::AssessmentResult;
    use crate::task::branch::{CheckpointDecision, DecompositionResult, SubtaskSpec};
    use crate::task::verify::{VerificationOutcome, VerificationResult};
    use crate::task::{MagnitudeEstimate, TaskPath};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[allow(clippy::struct_field_names)]
    struct MockAgentService {
        assess_responses: Mutex<VecDeque<AssessmentResult>>,
        leaf_responses: Mutex<VecDeque<TaskOutcome>>,
        decompose_responses: Mutex<VecDeque<DecompositionResult>>,
        verify_responses: Mutex<VecDeque<VerificationResult>>,
        checkpoint_responses: Mutex<VecDeque<CheckpointDecision>>,
        recovery_responses: Mutex<VecDeque<Option<String>>>,
    }

    impl MockAgentService {
        fn new() -> Self {
            Self {
                assess_responses: Mutex::new(VecDeque::new()),
                leaf_responses: Mutex::new(VecDeque::new()),
                decompose_responses: Mutex::new(VecDeque::new()),
                verify_responses: Mutex::new(VecDeque::new()),
                checkpoint_responses: Mutex::new(VecDeque::new()),
                recovery_responses: Mutex::new(VecDeque::new()),
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
        ) -> anyhow::Result<TaskOutcome> {
            Ok(self
                .leaf_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("no leaf response queued"))
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
            .push_back(TaskOutcome::Success);

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
            .push_back(TaskOutcome::Success);
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(TaskOutcome::Success);

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
                .push_back(TaskOutcome::Failed {
                    reason: "haiku failed".into(),
                });
        }
        mock.leaf_responses
            .lock()
            .unwrap()
            .push_back(TaskOutcome::Success);

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
                .push_back(TaskOutcome::Failed {
                    reason: "persistent failure".into(),
                });
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
            .push_back(TaskOutcome::Success);

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
            .push_back(TaskOutcome::Success);

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
            .push_back(TaskOutcome::Success);

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
            .push_back(TaskOutcome::Success);

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
            .push_back(TaskOutcome::Success);

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
}
