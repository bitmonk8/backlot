// ProblemSolverTask: assess -> execute (leaf or branch) -> verify.

pub mod assess;
pub mod branch;
pub mod leaf;
pub mod scope;
pub mod verify;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub u64);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "T{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPath {
    Leaf,
    Branch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPhase {
    Pending,
    Assessing,
    Executing,
    Verifying,
    Completed,
    Failed,
}

impl TaskPhase {
    #[allow(dead_code)]
    pub fn try_transition(self, new: Self) -> Result<Self, String> {
        if new == Self::Failed {
            return Ok(new);
        }
        let valid = matches!(
            (self, new),
            (Self::Pending, Self::Assessing)
                | (Self::Assessing | Self::Verifying, Self::Executing)
                | (Self::Executing, Self::Executing | Self::Verifying)
                | (Self::Verifying, Self::Completed)
        );
        if valid {
            Ok(new)
        } else {
            Err(format!("{self:?} -> {new:?} is not a valid transition"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Model {
    Haiku,
    Sonnet,
    Opus,
}

impl Model {
    /// Returns the next tier up, or `None` if already at the highest tier.
    pub const fn escalate(self) -> Option<Self> {
        match self {
            Self::Haiku => Some(Self::Sonnet),
            Self::Sonnet => Some(Self::Opus),
            Self::Opus => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub model: Model,
    pub succeeded: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MagnitudeEstimate {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct Magnitude {
    pub max_lines_added: u64,
    pub max_lines_modified: u64,
    pub max_lines_deleted: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
    pub api_calls: u32,
    pub total_tool_calls: u32,
    pub total_latency_ms: u64,
}

impl TaskUsage {
    pub const fn zero() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
            api_calls: 0,
            total_tool_calls: 0,
            total_latency_ms: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn accumulate(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
        cost_usd: f64,
        tool_calls: u32,
        latency_ms: u64,
    ) {
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.cache_creation_input_tokens += cache_creation_input_tokens;
        self.cache_read_input_tokens += cache_read_input_tokens;
        self.cost_usd += cost_usd;
        self.api_calls += 1;
        self.total_tool_calls += tool_calls;
        self.total_latency_ms += latency_ms;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskOutcome {
    Success,
    Failed { reason: String },
}

/// Result of a leaf execution: outcome plus any discoveries the agent reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafResult {
    pub outcome: TaskOutcome,
    pub discoveries: Vec<String>,
}

/// Recovery plan produced by the Opus recovery agent after a child failure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // `rationale` field used in wire format output and tests.
pub struct RecoveryPlan {
    /// If true, remaining pending children are superseded; only recovery subtasks run.
    pub full_redecomposition: bool,
    pub subtasks: Vec<branch::SubtaskSpec>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct Task {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub goal: String,
    pub verification_criteria: Vec<String>,
    pub path: Option<TaskPath>,
    pub phase: TaskPhase,
    pub current_model: Option<Model>,
    pub attempts: Vec<Attempt>,
    pub subtask_ids: Vec<TaskId>,
    pub magnitude_estimate: Option<MagnitudeEstimate>,
    pub magnitude: Option<Magnitude>,
    pub discoveries: Vec<String>,
    pub checkpoint_guidance: Option<String>,
    pub fix_attempts: Vec<Attempt>,
    pub decomposition_rationale: Option<String>,
    pub depth: u32,
    pub verification_fix_rounds: u32,
    pub is_fix_task: bool,
    pub recovery_rounds: u32,
    #[serde(default)]
    pub usage: TaskUsage,
}

impl Task {
    pub const fn new(
        id: TaskId,
        parent_id: Option<TaskId>,
        goal: String,
        verification_criteria: Vec<String>,
        depth: u32,
    ) -> Self {
        Self {
            id,
            parent_id,
            goal,
            verification_criteria,
            path: None,
            phase: TaskPhase::Pending,
            current_model: None,
            attempts: Vec::new(),
            subtask_ids: Vec::new(),
            magnitude_estimate: None,
            magnitude: None,
            discoveries: Vec::new(),
            checkpoint_guidance: None,
            fix_attempts: Vec::new(),
            decomposition_rationale: None,
            depth,
            verification_fix_rounds: 0,
            is_fix_task: false,
            recovery_rounds: 0,
            usage: TaskUsage::zero(),
        }
    }

    pub const fn set_assessment(
        &mut self,
        path: TaskPath,
        model: Model,
        magnitude: Option<Magnitude>,
    ) {
        self.path = Some(path);
        self.current_model = Some(model);
        self.magnitude = magnitude;
    }

    pub fn record_attempt(&mut self, attempt: Attempt, is_fix: bool) {
        if is_fix {
            self.fix_attempts.push(attempt);
        } else {
            self.attempts.push(attempt);
        }
    }

    /// Extend task discoveries and return the count added.
    pub fn record_discoveries(&mut self, discoveries: Vec<String>) -> usize {
        let count = discoveries.len();
        self.discoveries.extend(discoveries);
        count
    }

    pub const fn set_model(&mut self, model: Model) {
        self.current_model = Some(model);
    }

    pub fn set_decomposition_rationale(&mut self, rationale: String) {
        self.decomposition_rationale = Some(rationale);
    }

    pub fn set_checkpoint_guidance(&mut self, guidance: Option<String>) {
        self.checkpoint_guidance = guidance;
    }

    pub fn append_checkpoint_guidance(&mut self, new_guidance: &str) {
        self.checkpoint_guidance = Some(self.checkpoint_guidance.take().map_or_else(
            || new_guidance.to_owned(),
            |existing| format!("{existing}\n{new_guidance}"),
        ));
    }

    pub const fn increment_fix_rounds(&mut self) -> u32 {
        self.verification_fix_rounds += 1;
        self.verification_fix_rounds
    }

    pub const fn increment_recovery_rounds(&mut self) -> u32 {
        self.recovery_rounds += 1;
        self.recovery_rounds
    }

    pub fn accumulate_usage(&mut self, meta: &crate::agent::SessionMeta) {
        self.usage.accumulate(
            meta.input_tokens,
            meta.output_tokens,
            meta.cache_creation_input_tokens,
            meta.cache_read_input_tokens,
            meta.cost_usd,
            meta.tool_calls,
            meta.total_latency_ms,
        );
    }

    /// Count consecutive trailing attempts at the given model tier.
    pub fn trailing_attempts_at_tier(&self, model: Model, is_fix: bool) -> u32 {
        let list = if is_fix {
            &self.fix_attempts
        } else {
            &self.attempts
        };
        #[allow(clippy::cast_possible_truncation)]
        let count = list.iter().rev().take_while(|a| a.model == model).count() as u32;
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ordering_haiku_lt_sonnet_lt_opus() {
        assert!(Model::Haiku < Model::Sonnet);
        assert!(Model::Sonnet < Model::Opus);
        assert!(Model::Haiku < Model::Opus);
    }

    #[test]
    fn task_phase_valid_transitions() {
        let cases = [
            (TaskPhase::Pending, TaskPhase::Assessing),
            (TaskPhase::Assessing, TaskPhase::Executing),
            (TaskPhase::Executing, TaskPhase::Executing),
            (TaskPhase::Executing, TaskPhase::Verifying),
            (TaskPhase::Verifying, TaskPhase::Completed),
            (TaskPhase::Verifying, TaskPhase::Executing),
        ];
        for (from, to) in cases {
            assert_eq!(from.try_transition(to), Ok(to), "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn task_phase_any_to_failed() {
        let all = [
            TaskPhase::Pending,
            TaskPhase::Assessing,
            TaskPhase::Executing,
            TaskPhase::Verifying,
            TaskPhase::Completed,
            TaskPhase::Failed,
        ];
        for phase in all {
            assert_eq!(
                phase.try_transition(TaskPhase::Failed),
                Ok(TaskPhase::Failed)
            );
        }
    }

    #[test]
    fn task_phase_invalid_transitions() {
        let cases = [
            (TaskPhase::Pending, TaskPhase::Executing),
            (TaskPhase::Pending, TaskPhase::Completed),
            (TaskPhase::Assessing, TaskPhase::Verifying),
            (TaskPhase::Executing, TaskPhase::Completed),
            (TaskPhase::Completed, TaskPhase::Pending),
        ];
        for (from, to) in cases {
            assert!(
                from.try_transition(to).is_err(),
                "{from:?} -> {to:?} should fail"
            );
        }
    }

    #[test]
    fn task_new_defaults() {
        let t = Task::new(TaskId(1), None, String::new(), Vec::new(), 0);
        assert_eq!(t.id, TaskId(1));
        assert_eq!(t.parent_id, None);
        assert_eq!(t.phase, TaskPhase::Pending);
        assert_eq!(t.path, None);
        assert_eq!(t.current_model, None);
        assert!(t.attempts.is_empty());
        assert!(t.subtask_ids.is_empty());
        assert_eq!(t.magnitude_estimate, None);
        assert_eq!(t.depth, 0);
        assert_eq!(t.verification_fix_rounds, 0);
        assert!(!t.is_fix_task);
        assert_eq!(t.recovery_rounds, 0);
    }

    #[test]
    fn model_escalate_chain() {
        assert_eq!(Model::Haiku.escalate(), Some(Model::Sonnet));
        assert_eq!(Model::Sonnet.escalate(), Some(Model::Opus));
        assert_eq!(Model::Opus.escalate(), None);
    }

    #[test]
    fn recovery_plan_equality() {
        let spec = branch::SubtaskSpec {
            goal: "fix it".into(),
            verification_criteria: vec!["works".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        };
        let a = RecoveryPlan {
            full_redecomposition: false,
            subtasks: vec![spec],
            rationale: "reason".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);

        let c = RecoveryPlan {
            full_redecomposition: true,
            subtasks: Vec::new(),
            rationale: "other".into(),
        };
        assert_ne!(a, c);
    }

    #[test]
    fn task_id_display() {
        assert_eq!(TaskId(0).to_string(), "T0");
        assert_eq!(TaskId(42).to_string(), "T42");
    }
}
