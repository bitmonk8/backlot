// ProblemSolverTask: assess -> execute (leaf or branch) -> verify.

pub mod assess;
pub mod branch;
pub mod leaf;
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
        }
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
            assert_eq!(phase.try_transition(TaskPhase::Failed), Ok(TaskPhase::Failed));
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
            assert!(from.try_transition(to).is_err(), "{from:?} -> {to:?} should fail");
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
    fn magnitude_estimate_equality() {
        assert_eq!(MagnitudeEstimate::Small, MagnitudeEstimate::Small);
        assert_ne!(MagnitudeEstimate::Small, MagnitudeEstimate::Large);
    }

    #[test]
    fn task_path_equality() {
        assert_eq!(TaskPath::Leaf, TaskPath::Leaf);
        assert_eq!(TaskPath::Branch, TaskPath::Branch);
        assert_ne!(TaskPath::Leaf, TaskPath::Branch);
    }

    #[test]
    fn leaf_result_equality() {
        let a = LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["found X".into()],
        };
        let b = LeafResult {
            outcome: TaskOutcome::Success,
            discoveries: vec!["found X".into()],
        };
        let c = LeafResult {
            outcome: TaskOutcome::Failed { reason: "oops".into() },
            discoveries: Vec::new(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
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
