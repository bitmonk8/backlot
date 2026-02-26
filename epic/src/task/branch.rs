// Branch execution path: design + decompose -> execute children -> verify aggregate.

use super::MagnitudeEstimate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
