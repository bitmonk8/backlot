use crate::agent::{AgentResult, AgentService, SessionMeta, TaskContext};
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{LeafResult, Model, RecoveryPlan, TaskId};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

#[allow(clippy::struct_field_names)]
pub struct MockAgentService {
    pub assess_responses: Mutex<VecDeque<AssessmentResult>>,
    pub leaf_responses: Mutex<VecDeque<LeafResult>>,
    pub fix_leaf_responses: Mutex<VecDeque<LeafResult>>,
    pub decompose_responses: Mutex<VecDeque<DecompositionResult>>,
    pub fix_subtask_responses: Mutex<VecDeque<DecompositionResult>>,
    pub verify_responses: Mutex<VecDeque<VerificationResult>>,
    pub file_level_review_responses: Mutex<VecDeque<VerificationResult>>,
    pub checkpoint_responses: Mutex<VecDeque<CheckpointDecision>>,
    pub checkpoint_errors: Mutex<VecDeque<String>>,
    pub verify_errors: Mutex<HashMap<TaskId, VecDeque<Option<String>>>>,
    pub fix_subtask_errors: Mutex<HashMap<TaskId, VecDeque<Option<String>>>>,
    pub recovery_responses: Mutex<VecDeque<Option<String>>>,
    pub recovery_plan_responses: Mutex<VecDeque<RecoveryPlan>>,
    pub verify_models: Mutex<Vec<Model>>,
    pub decompose_models: Mutex<Vec<Model>>,
}

impl MockAgentService {
    pub fn new() -> Self {
        Self {
            assess_responses: Mutex::new(VecDeque::new()),
            leaf_responses: Mutex::new(VecDeque::new()),
            fix_leaf_responses: Mutex::new(VecDeque::new()),
            decompose_responses: Mutex::new(VecDeque::new()),
            fix_subtask_responses: Mutex::new(VecDeque::new()),
            verify_responses: Mutex::new(VecDeque::new()),
            file_level_review_responses: Mutex::new(VecDeque::new()),
            checkpoint_responses: Mutex::new(VecDeque::new()),
            checkpoint_errors: Mutex::new(VecDeque::new()),
            verify_errors: Mutex::new(HashMap::new()),
            fix_subtask_errors: Mutex::new(HashMap::new()),
            recovery_responses: Mutex::new(VecDeque::new()),
            recovery_plan_responses: Mutex::new(VecDeque::new()),
            verify_models: Mutex::new(Vec::new()),
            decompose_models: Mutex::new(Vec::new()),
        }
    }

    pub fn push_verify_errors(&self, id: TaskId, errors: Vec<Option<String>>) {
        self.verify_errors
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .extend(errors);
    }

    pub fn push_fix_subtask_errors(&self, id: TaskId, errors: Vec<Option<String>>) {
        self.fix_subtask_errors
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .extend(errors);
    }
}

/// Wrap a value in `AgentResult` with zero-cost `SessionMeta`.
fn mock_result<T>(value: T) -> AgentResult<T> {
    AgentResult {
        value,
        meta: SessionMeta::default(),
    }
}

/// Fluent builder for `MockAgentService`. Wraps queue construction to reduce
/// the per-response ceremony from 4 lines to 1 method call.
pub struct MockBuilder {
    inner: MockAgentService,
}

impl MockBuilder {
    pub fn new() -> Self {
        Self {
            inner: MockAgentService::new(),
        }
    }

    pub fn build(&mut self) -> MockAgentService {
        std::mem::replace(&mut self.inner, MockAgentService::new())
    }

    // -----------------------------------------------------------------------
    // Assessment
    // -----------------------------------------------------------------------

    pub fn assess_leaf(&mut self) -> &mut Self {
        self.inner
            .assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: crate::task::TaskPath::Leaf,
                model: Model::Haiku,
                rationale: "simple task".into(),
                magnitude: None,
            });
        self
    }

    pub fn assess_branch(&mut self) -> &mut Self {
        self.inner
            .assess_responses
            .lock()
            .unwrap()
            .push_back(AssessmentResult {
                path: crate::task::TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "needs decomposition".into(),
                magnitude: None,
            });
        self
    }

    // -----------------------------------------------------------------------
    // Leaf execution
    // -----------------------------------------------------------------------

    pub fn leaf_success(&mut self) -> &mut Self {
        self.inner
            .leaf_responses
            .lock()
            .unwrap()
            .push_back(LeafResult {
                outcome: crate::task::TaskOutcome::Success,
                discoveries: Vec::new(),
            });
        self
    }

    pub fn leaf_success_with_discoveries(&mut self, discoveries: Vec<String>) -> &mut Self {
        self.inner
            .leaf_responses
            .lock()
            .unwrap()
            .push_back(LeafResult {
                outcome: crate::task::TaskOutcome::Success,
                discoveries,
            });
        self
    }

    pub fn leaf_failed(&mut self, reason: &str) -> &mut Self {
        self.inner
            .leaf_responses
            .lock()
            .unwrap()
            .push_back(LeafResult {
                outcome: crate::task::TaskOutcome::Failed {
                    reason: reason.into(),
                },
                discoveries: Vec::new(),
            });
        self
    }

    pub fn leaf_failures(&mut self, count: usize, reason: &str) -> &mut Self {
        for _ in 0..count {
            self.leaf_failed(reason);
        }
        self
    }

    // -----------------------------------------------------------------------
    // Fix leaf execution
    // -----------------------------------------------------------------------

    pub fn fix_leaf_success(&mut self) -> &mut Self {
        self.inner
            .fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(LeafResult {
                outcome: crate::task::TaskOutcome::Success,
                discoveries: Vec::new(),
            });
        self
    }

    pub fn fix_leaf_failed(&mut self, reason: &str) -> &mut Self {
        self.inner
            .fix_leaf_responses
            .lock()
            .unwrap()
            .push_back(LeafResult {
                outcome: crate::task::TaskOutcome::Failed {
                    reason: reason.into(),
                },
                discoveries: Vec::new(),
            });
        self
    }

    pub fn fix_leaf_failures(&mut self, count: usize, reason: &str) -> &mut Self {
        for _ in 0..count {
            self.fix_leaf_failed(reason);
        }
        self
    }

    // -----------------------------------------------------------------------
    // Verification
    // -----------------------------------------------------------------------

    pub fn verify_pass(&mut self) -> &mut Self {
        self.inner.verify_responses.lock().unwrap().push_back(
            crate::task::verify::VerificationResult {
                outcome: crate::task::verify::VerificationOutcome::Pass,
                details: "all checks passed".into(),
            },
        );
        self
    }

    pub fn verify_passes(&mut self, count: usize) -> &mut Self {
        for _ in 0..count {
            self.verify_pass();
        }
        self
    }

    pub fn verify_fail(&mut self, reason: &str) -> &mut Self {
        self.inner.verify_responses.lock().unwrap().push_back(
            crate::task::verify::VerificationResult {
                outcome: crate::task::verify::VerificationOutcome::Fail {
                    reason: reason.into(),
                },
                details: "check failed".into(),
            },
        );
        self
    }

    pub fn verify_error(&mut self, task_id: TaskId, msg: &str) -> &mut Self {
        self.inner
            .push_verify_errors(task_id, vec![Some(msg.into())]);
        self
    }

    pub fn verify_errors_sequence(
        &mut self,
        task_id: TaskId,
        errors: Vec<Option<String>>,
    ) -> &mut Self {
        self.inner.push_verify_errors(task_id, errors);
        self
    }

    // -----------------------------------------------------------------------
    // File-level review
    // -----------------------------------------------------------------------

    pub fn file_review_pass(&mut self) -> &mut Self {
        self.inner
            .file_level_review_responses
            .lock()
            .unwrap()
            .push_back(crate::task::verify::VerificationResult {
                outcome: crate::task::verify::VerificationOutcome::Pass,
                details: "file-level review passed".into(),
            });
        self
    }

    pub fn file_review_passes(&mut self, count: usize) -> &mut Self {
        for _ in 0..count {
            self.file_review_pass();
        }
        self
    }

    pub fn file_review_fail(&mut self, reason: &str) -> &mut Self {
        self.inner
            .file_level_review_responses
            .lock()
            .unwrap()
            .push_back(crate::task::verify::VerificationResult {
                outcome: crate::task::verify::VerificationOutcome::Fail {
                    reason: reason.into(),
                },
                details: "file-level review failed".into(),
            });
        self
    }

    // -----------------------------------------------------------------------
    // Decomposition
    // -----------------------------------------------------------------------

    pub fn decompose(&mut self, result: crate::task::branch::DecompositionResult) -> &mut Self {
        self.inner
            .decompose_responses
            .lock()
            .unwrap()
            .push_back(result);
        self
    }

    pub fn decompose_one(&mut self) -> &mut Self {
        self.decompose(crate::task::branch::DecompositionResult {
            subtasks: vec![crate::task::branch::SubtaskSpec {
                goal: "child task".into(),
                verification_criteria: vec!["child passes".into()],
                magnitude_estimate: crate::task::MagnitudeEstimate::Small,
            }],
            rationale: "single subtask".into(),
        })
    }

    pub fn decompose_two(&mut self) -> &mut Self {
        self.decompose(crate::task::branch::DecompositionResult {
            subtasks: vec![
                crate::task::branch::SubtaskSpec {
                    goal: "child A".into(),
                    verification_criteria: vec!["A passes".into()],
                    magnitude_estimate: crate::task::MagnitudeEstimate::Small,
                },
                crate::task::branch::SubtaskSpec {
                    goal: "child B".into(),
                    verification_criteria: vec!["B passes".into()],
                    magnitude_estimate: crate::task::MagnitudeEstimate::Small,
                },
            ],
            rationale: "two subtasks".into(),
        })
    }

    pub fn decompose_three(&mut self) -> &mut Self {
        self.decompose(crate::task::branch::DecompositionResult {
            subtasks: vec![
                crate::task::branch::SubtaskSpec {
                    goal: "child A".into(),
                    verification_criteria: vec!["A passes".into()],
                    magnitude_estimate: crate::task::MagnitudeEstimate::Small,
                },
                crate::task::branch::SubtaskSpec {
                    goal: "child B".into(),
                    verification_criteria: vec!["B passes".into()],
                    magnitude_estimate: crate::task::MagnitudeEstimate::Small,
                },
                crate::task::branch::SubtaskSpec {
                    goal: "child C".into(),
                    verification_criteria: vec!["C passes".into()],
                    magnitude_estimate: crate::task::MagnitudeEstimate::Small,
                },
            ],
            rationale: "three subtasks".into(),
        })
    }

    // -----------------------------------------------------------------------
    // Fix subtasks
    // -----------------------------------------------------------------------

    pub fn fix_subtask_one(&mut self) -> &mut Self {
        self.inner.fix_subtask_responses.lock().unwrap().push_back(
            crate::task::branch::DecompositionResult {
                subtasks: vec![crate::task::branch::SubtaskSpec {
                    goal: "fix subtask".into(),
                    verification_criteria: vec!["fix passes".into()],
                    magnitude_estimate: crate::task::MagnitudeEstimate::Small,
                }],
                rationale: "targeted fix".into(),
            },
        );
        self
    }

    pub fn fix_subtask_error(&mut self, task_id: TaskId, msg: &str) -> &mut Self {
        self.inner
            .push_fix_subtask_errors(task_id, vec![Some(msg.into())]);
        self
    }

    pub fn fix_subtask_errors(
        &mut self,
        task_id: TaskId,
        errors: Vec<Option<String>>,
    ) -> &mut Self {
        self.inner.push_fix_subtask_errors(task_id, errors);
        self
    }

    // -----------------------------------------------------------------------
    // Checkpoint
    // -----------------------------------------------------------------------

    pub fn checkpoint_proceed(&mut self) -> &mut Self {
        self.inner
            .checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Proceed);
        self
    }

    pub fn checkpoint_adjust(&mut self, guidance: &str) -> &mut Self {
        self.inner
            .checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Adjust {
                guidance: guidance.into(),
            });
        self
    }

    pub fn checkpoint_escalate(&mut self) -> &mut Self {
        self.inner
            .checkpoint_responses
            .lock()
            .unwrap()
            .push_back(CheckpointDecision::Escalate);
        self
    }

    pub fn checkpoint_error(&mut self, msg: &str) -> &mut Self {
        self.inner
            .checkpoint_errors
            .lock()
            .unwrap()
            .push_back(msg.into());
        self
    }

    // -----------------------------------------------------------------------
    // Recovery
    // -----------------------------------------------------------------------

    pub fn recovery_unrecoverable(&mut self) -> &mut Self {
        self.inner
            .recovery_responses
            .lock()
            .unwrap()
            .push_back(None);
        self
    }

    pub fn recovery_recoverable(&mut self, strategy: &str) -> &mut Self {
        self.inner
            .recovery_responses
            .lock()
            .unwrap()
            .push_back(Some(strategy.into()));
        self
    }

    pub fn recovery_plan(&mut self, plan: RecoveryPlan) -> &mut Self {
        self.inner
            .recovery_plan_responses
            .lock()
            .unwrap()
            .push_back(plan);
        self
    }

    pub fn recovery_plan_incremental(&mut self) -> &mut Self {
        self.recovery_plan(RecoveryPlan {
            full_redecomposition: false,
            subtasks: vec![crate::task::branch::SubtaskSpec {
                goal: "recovery fix".into(),
                verification_criteria: vec!["fix works".into()],
                magnitude_estimate: crate::task::MagnitudeEstimate::Small,
            }],
            rationale: "incremental recovery".into(),
        })
    }

    pub fn recovery_plan_full(&mut self) -> &mut Self {
        self.recovery_plan(RecoveryPlan {
            full_redecomposition: true,
            subtasks: vec![crate::task::branch::SubtaskSpec {
                goal: "full redo".into(),
                verification_criteria: vec!["redo works".into()],
                magnitude_estimate: crate::task::MagnitudeEstimate::Medium,
            }],
            rationale: "full re-decomposition".into(),
        })
    }
}

impl AgentService for MockAgentService {
    async fn assess(&self, _ctx: &TaskContext) -> anyhow::Result<AgentResult<AssessmentResult>> {
        self.assess_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no assess response queued"))
    }

    async fn execute_leaf(
        &self,
        _ctx: &TaskContext,
        _model: Model,
    ) -> anyhow::Result<AgentResult<LeafResult>> {
        self.leaf_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no leaf response queued"))
    }

    async fn fix_leaf(
        &self,
        _ctx: &TaskContext,
        _model: Model,
        _failure_reason: &str,
        _attempt: u32,
    ) -> anyhow::Result<AgentResult<LeafResult>> {
        self.fix_leaf_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no fix_leaf response queued"))
    }

    async fn design_and_decompose(
        &self,
        _ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<AgentResult<DecompositionResult>> {
        self.decompose_models.lock().unwrap().push(model);
        self.decompose_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no decompose response queued"))
    }

    async fn design_fix_subtasks(
        &self,
        ctx: &TaskContext,
        _model: Model,
        _verification_issues: &str,
        _round: u32,
    ) -> anyhow::Result<AgentResult<DecompositionResult>> {
        let injected = self
            .fix_subtask_errors
            .lock()
            .unwrap()
            .get_mut(&ctx.task.id)
            .and_then(VecDeque::pop_front);
        if let Some(Some(msg)) = injected {
            return Err(anyhow::anyhow!(msg));
        }
        self.fix_subtask_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no fix_subtask response queued"))
    }

    async fn verify(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<AgentResult<VerificationResult>> {
        let injected = self
            .verify_errors
            .lock()
            .unwrap()
            .get_mut(&ctx.task.id)
            .and_then(VecDeque::pop_front);
        if let Some(Some(msg)) = injected {
            return Err(anyhow::anyhow!(msg));
        }
        self.verify_models.lock().unwrap().push(model);
        self.verify_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no verify response queued"))
    }

    async fn file_level_review(
        &self,
        _ctx: &TaskContext,
        _model: Model,
    ) -> anyhow::Result<AgentResult<VerificationResult>> {
        self.file_level_review_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no file_level_review response queued"))
    }

    async fn checkpoint(
        &self,
        _ctx: &TaskContext,
        _discoveries: &[String],
    ) -> anyhow::Result<AgentResult<CheckpointDecision>> {
        let front = self.checkpoint_errors.lock().unwrap().pop_front();
        if let Some(msg) = front {
            return Err(anyhow::anyhow!(msg));
        }
        self.checkpoint_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no checkpoint response queued"))
    }

    async fn assess_recovery(
        &self,
        _ctx: &TaskContext,
        _failure_reason: &str,
    ) -> anyhow::Result<AgentResult<Option<String>>> {
        self.recovery_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no recovery response queued"))
    }

    async fn design_recovery_subtasks(
        &self,
        _ctx: &TaskContext,
        _failure_reason: &str,
        _strategy: &str,
        _recovery_round: u32,
    ) -> anyhow::Result<AgentResult<RecoveryPlan>> {
        self.recovery_plan_responses
            .lock()
            .unwrap()
            .pop_front()
            .map(mock_result)
            .ok_or_else(|| anyhow::anyhow!("no recovery_plan response queued"))
    }
}
