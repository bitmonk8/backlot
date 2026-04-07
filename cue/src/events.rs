// Event types for orchestrator-to-presentation communication.
// CueEvent contains only the orchestration events emitted by the orchestrator.
// Application crates define their own full event enums and map via From<CueEvent>.

use crate::types::{Model, TaskId, TaskOutcome, TaskPath, TaskPhase};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CueEvent {
    TaskRegistered {
        task_id: TaskId,
        parent_id: Option<TaskId>,
        goal: String,
        depth: u32,
    },
    PhaseTransition {
        task_id: TaskId,
        phase: TaskPhase,
    },
    PathSelected {
        task_id: TaskId,
        path: TaskPath,
    },
    ModelSelected {
        task_id: TaskId,
        model: Model,
    },
    SubtasksCreated {
        parent_id: TaskId,
        child_ids: Vec<TaskId>,
    },
    TaskCompleted {
        task_id: TaskId,
        outcome: TaskOutcome,
    },
    TaskLimitReached {
        task_id: TaskId,
    },
    BranchFixRound {
        task_id: TaskId,
        round: u32,
        model: Model,
    },
    FixSubtasksCreated {
        task_id: TaskId,
        count: usize,
        round: u32,
    },
    RecoverySubtasksCreated {
        task_id: TaskId,
        count: usize,
        round: u32,
    },
}
