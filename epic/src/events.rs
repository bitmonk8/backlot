// Event system for orchestrator-to-TUI communication.

use crate::task::{Model, TaskId, TaskOutcome, TaskPath, TaskPhase};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum Event {
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
    ModelEscalated {
        task_id: TaskId,
        from: Model,
        to: Model,
    },
    SubtasksCreated {
        parent_id: TaskId,
        child_ids: Vec<TaskId>,
    },
    TaskCompleted {
        task_id: TaskId,
        outcome: TaskOutcome,
    },
    RetryAttempt {
        task_id: TaskId,
        attempt: u32,
        model: Model,
    },
    DiscoveriesRecorded {
        task_id: TaskId,
        count: usize,
    },
    CheckpointAdjust {
        task_id: TaskId,
    },
    CheckpointEscalate {
        task_id: TaskId,
    },
    FixAttempt {
        task_id: TaskId,
        attempt: u32,
        model: Model,
    },
    FixModelEscalated {
        task_id: TaskId,
        from: Model,
        to: Model,
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
    FileLevelReviewCompleted {
        task_id: TaskId,
        passed: bool,
    },
    RecoveryStarted {
        task_id: TaskId,
        round: u32,
    },
    RecoveryPlanSelected {
        task_id: TaskId,
        approach: String,
    },
    RecoverySubtasksCreated {
        task_id: TaskId,
        count: usize,
        round: u32,
    },
    TaskLimitReached {
        task_id: TaskId,
    },
    UsageUpdated {
        task_id: TaskId,
        phase_cost_usd: f64,
        total_cost_usd: f64,
    },
    VaultBootstrapCompleted {
        cost_usd: f64,
    },
    VaultRecorded {
        task_id: TaskId,
        document: String,
    },
    VaultReorganizeCompleted {
        merged: usize,
        restructured: usize,
        deleted: usize,
    },
}

pub type EventSender = mpsc::UnboundedSender<Event>;
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

pub fn event_channel() -> (EventSender, EventReceiver) {
    mpsc::unbounded_channel()
}
