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
