// Orchestration protocol types: identity, state machine, outcomes, data.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafResult {
    pub outcome: TaskOutcome,
    pub discoveries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    pub full_redecomposition: bool,
    pub subtasks: Vec<SubtaskSpec>,
    pub rationale: String,
}

/// Metadata from a single agent session.
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
    pub tool_calls: u32,
    pub total_latency_ms: u64,
}

// Centralized accumulation so that adding a new numeric field to `SessionMeta`
// only requires updating this impl, eliminating the silent-data-loss risk of
// per-field `+=` enumerations at every call site.
impl std::ops::AddAssign<&Self> for SessionMeta {
    fn add_assign(&mut self, rhs: &Self) {
        // Exhaustive destructure (no `..` rest pattern) of the rhs so that
        // adding a new field to `SessionMeta` produces a compile error here
        // (E0027: "pattern does not mention field"), forcing the new field
        // to be folded into the accumulation.
        let Self {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cost_usd,
            tool_calls,
            total_latency_ms,
        } = rhs;
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.cache_creation_input_tokens += cache_creation_input_tokens;
        self.cache_read_input_tokens += cache_read_input_tokens;
        self.cost_usd += cost_usd;
        self.tool_calls += tool_calls;
        self.total_latency_ms += total_latency_ms;
    }
}

impl std::ops::AddAssign for SessionMeta {
    fn add_assign(&mut self, rhs: Self) {
        *self += &rhs;
    }
}

/// Agent call result with observability metadata.
#[derive(Debug)]
pub struct AgentResult<T> {
    pub value: T,
    pub meta: SessionMeta,
}

// --- Assessment ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessmentResult {
    pub path: TaskPath,
    pub model: Model,
    pub rationale: String,
    pub magnitude: Option<Magnitude>,
}

// --- Verification ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationOutcome {
    Pass,
    Fail { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub outcome: VerificationOutcome,
    pub details: String,
}

pub enum VerifyOutcome {
    Passed,
    Failed(String),
}

// --- Subtask specification ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtaskSpec {
    pub goal: String,
    pub verification_criteria: Vec<String>,
    pub magnitude_estimate: MagnitudeEstimate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionResult {
    pub subtasks: Vec<SubtaskSpec>,
    pub rationale: String,
}

// --- Scope ---

#[derive(Debug, PartialEq, Eq)]
pub enum ScopeCheck {
    WithinBounds,
    Exceeded {
        metric: String,
        actual: u64,
        limit: u64,
    },
}

// --- Checkpoint ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointDecision {
    Proceed,
    Adjust { guidance: String },
    Escalate,
}

// --- Branch outcomes ---

pub enum BranchVerifyOutcome {
    Passed,
    Failed { reason: String },
    FailedNoFixLoop { reason: String },
}

#[derive(Debug)]
pub enum FixBudgetCheck {
    WithinBudget { model: Model },
    Exhausted,
}

pub enum RecoveryDecision {
    Unrecoverable {
        reason: String,
    },
    Plan {
        specs: Vec<SubtaskSpec>,
        supersede_pending: bool,
    },
}

pub enum ChildResponse {
    Continue,
    NeedRecoverySubtasks {
        specs: Vec<SubtaskSpec>,
        supersede_pending: bool,
    },
    Failed(String),
}

// --- Resume / registration / recovery eligibility ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePoint {
    Terminal(TaskOutcome),
    LeafExecuting,
    LeafVerifying,
    BranchExecuting,
    BranchVerifying,
    NeedAssessment,
}

#[derive(Debug, Clone)]
pub struct RegistrationInfo {
    pub parent_id: Option<TaskId>,
    pub goal: String,
    pub depth: u32,
    pub phase: TaskPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryEligibility {
    NotEligible { reason: String },
    Eligible { round: u32 },
}

// --- Context types ---

/// Summary of a completed sibling task, provided as context to agent calls.
#[derive(Debug, Clone)]
pub struct SiblingSummary {
    pub id: TaskId,
    pub goal: String,
    pub outcome: TaskOutcome,
    pub discoveries: Vec<String>,
}

/// Status of a child subtask.
#[derive(Debug, Clone)]
pub enum ChildStatus {
    Completed,
    Failed { reason: String },
    Pending,
    InProgress,
}

/// Summary of a child subtask, used in branch-task context.
#[derive(Debug, Clone)]
pub struct ChildSummary {
    pub goal: String,
    pub status: ChildStatus,
    pub discoveries: Vec<String>,
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
    fn model_escalate_chain() {
        assert_eq!(Model::Haiku.escalate(), Some(Model::Sonnet));
        assert_eq!(Model::Sonnet.escalate(), Some(Model::Opus));
        assert_eq!(Model::Opus.escalate(), None);
    }

    #[test]
    fn task_id_display() {
        assert_eq!(TaskId(0).to_string(), "T0");
        assert_eq!(TaskId(42).to_string(), "T42");
    }

    #[test]
    fn recovery_plan_equality() {
        let spec = SubtaskSpec {
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

    // Guard against silent data loss when adding a new numeric field to
    // `SessionMeta`: this test sums two distinct, non-overlapping metas and
    // asserts every field on the sum equals the expected total. If a new field
    // is added without updating `AddAssign`, the destructuring pattern in the
    // impl will fail to compile.
    #[test]
    fn session_meta_add_assign_sums_all_fields() {
        let mut a = SessionMeta {
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 4,
            cost_usd: 5.5,
            tool_calls: 6,
            total_latency_ms: 7,
        };
        let b = SessionMeta {
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_input_tokens: 30,
            cache_read_input_tokens: 40,
            cost_usd: 50.25,
            tool_calls: 60,
            total_latency_ms: 70,
        };
        a += &b;
        assert_eq!(a.input_tokens, 11);
        assert_eq!(a.output_tokens, 22);
        assert_eq!(a.cache_creation_input_tokens, 33);
        assert_eq!(a.cache_read_input_tokens, 44);
        assert!((a.cost_usd - 55.75).abs() < f64::EPSILON);
        assert_eq!(a.tool_calls, 66);
        assert_eq!(a.total_latency_ms, 77);

        // Verify owned-rhs (`AddAssign<Self>`) sums every field, not just one,
        // so a future change that stops delegating to `AddAssign<&Self>` cannot
        // silently drop fields.
        let mut c = SessionMeta::default();
        c += b;
        assert_eq!(c.input_tokens, 10);
        assert_eq!(c.output_tokens, 20);
        assert_eq!(c.cache_creation_input_tokens, 30);
        assert_eq!(c.cache_read_input_tokens, 40);
        assert!((c.cost_usd - 50.25).abs() < f64::EPSILON);
        assert_eq!(c.tool_calls, 60);
        assert_eq!(c.total_latency_ms, 70);
    }
}
