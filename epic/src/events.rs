// Event system for orchestrator-to-TUI communication.

use crate::task::{Model, TaskId, TaskOutcome, TaskPath, TaskPhase};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants read by TUI (not yet wired).
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
    VerificationStarted {
        task_id: TaskId,
    },
    VerificationComplete {
        task_id: TaskId,
        passed: bool,
    },
    DiscoveriesRecorded {
        task_id: TaskId,
        count: usize,
    },
}

pub type EventSender = mpsc::UnboundedSender<Event>;
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

pub fn event_channel() -> (EventSender, EventReceiver) {
    mpsc::unbounded_channel()
}
