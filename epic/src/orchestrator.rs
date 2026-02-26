// Recursive task execution, DFS traversal, state persistence, resume.

use crate::agent::{AgentService, SiblingSummary, TaskContext};
use crate::events::{Event, EventSender};
use crate::state::EpicState;
use crate::task::assess::AssessmentResult;
use crate::task::verify::VerificationOutcome;
use crate::task::{Attempt, Model, Task, TaskId, TaskOutcome, TaskPath, TaskPhase};
use std::future::Future;
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

#[allow(dead_code)] // Used in tests; main.rs wiring pending.
pub struct Orchestrator<A: AgentService> {
    agent: A,
    state: EpicState,
    events: EventSender,
}

impl<A: AgentService> Orchestrator<A> {
    pub const fn new(agent: A, state: EpicState, events: EventSender) -> Self {
        Self {
            agent,
            state,
            events,
        }
    }

    pub async fn run(&mut self, root_id: TaskId) -> Result<TaskOutcome, OrchestratorError> {
        self.execute_task(root_id).await
    }

    #[allow(dead_code)] // Used after run completes for state persistence.
    pub fn into_state(self) -> EpicState {
        self.state
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn transition(&mut self, id: TaskId, phase: TaskPhase) -> Result<(), OrchestratorError> {
        let task = self
            .state
            .get_mut(id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        task.phase = phase.clone();
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

            self.transition(id, TaskPhase::Executing)?;

            let outcome = match assessment.path {
                TaskPath::Leaf => self.execute_leaf(id).await?,
                TaskPath::Branch => self.execute_branch(id).await?,
            };

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
                        Ok(outcome)
                    }
                }
            } else {
                self.transition(id, TaskPhase::Failed)?;
                self.emit(Event::TaskCompleted {
                    task_id: id,
                    outcome: outcome.clone(),
                });
                Ok(outcome)
            }
        })
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

        let mut child_ids = Vec::new();
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
            child_ids.push(child_id);
            self.state.insert(child);
        }

        {
            let task = self
                .state
                .get_mut(id)
                .ok_or(OrchestratorError::TaskNotFound(id))?;
            task.subtask_ids.clone_from(&child_ids);
        }

        self.emit(Event::SubtasksCreated {
            parent_id: id,
            child_ids: child_ids.clone(),
        });

        for &child_id in &child_ids {
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
