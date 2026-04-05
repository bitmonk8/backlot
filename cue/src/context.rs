// Tree context: read-only snapshot of tree state around a task.

use crate::types::{ChildSummary, SiblingSummary};

/// Read-only snapshot of tree state around a task. Built by the store
/// before calling a task method. Owned data, not references into state.
#[derive(Debug, Clone)]
pub struct TreeContext {
    pub parent_goal: Option<String>,
    pub parent_decomposition_rationale: Option<String>,
    pub parent_discoveries: Vec<String>,
    pub ancestor_goals: Vec<String>,
    pub completed_siblings: Vec<SiblingSummary>,
    pub pending_sibling_goals: Vec<String>,
    pub children: Vec<ChildSummary>,
    pub checkpoint_guidance: Option<String>,
}
