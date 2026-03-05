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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone)]
pub struct LeafResult {
    pub outcome: TaskOutcome,
    pub discoveries: Vec<String>,
}

/// Recovery plan produced by the Opus recovery agent after a child failure.
#[derive(Debug, Clone)]
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
    pub model: Option<Model>,
    pub current_model: Option<Model>,
    pub attempts: Vec<Attempt>,
    pub subtask_ids: Vec<TaskId>,
    pub magnitude_estimate: Option<MagnitudeEstimate>,
    pub magnitude: Option<Magnitude>,
    pub discoveries: Vec<String>,
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
            model: None,
            current_model: None,
            attempts: Vec::new(),
            subtask_ids: Vec::new(),
            magnitude_estimate: None,
            magnitude: None,
            discoveries: Vec::new(),
            fix_attempts: Vec::new(),
            decomposition_rationale: None,
            depth,
            verification_fix_rounds: 0,
            is_fix_task: false,
            recovery_rounds: 0,
        }
    }
}
