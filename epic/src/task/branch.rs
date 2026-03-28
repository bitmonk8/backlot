// Branch execution path: design + decompose -> execute children -> verify aggregate.

use super::{MagnitudeEstimate, TaskOutcome};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointDecision {
    Proceed,
    Adjust { guidance: String },
    Escalate,
}

/// Response from a branch after a child completes.
#[allow(dead_code)]
pub enum ChildResponse {
    /// Proceed to next child.
    Continue,
    /// Child failed; branch designed recovery subtasks.
    NeedRecoverySubtasks {
        specs: Vec<SubtaskSpec>,
        /// If true, pending siblings are superseded (full redecomposition).
        supersede_pending: bool,
    },
    /// Unrecoverable failure.
    Failed(String),
}

/// Result from branch finalization (post-children verification).
#[allow(dead_code)]
pub enum BranchResult {
    /// Branch verified successfully.
    Complete(TaskOutcome),
    /// Verification failed; branch designed fix subtasks.
    NeedSubtasks(Vec<SubtaskSpec>),
    /// Terminal failure (fix budget exhausted).
    Failed(String),
}
