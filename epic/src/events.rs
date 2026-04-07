// Event system: full event enum, EventLog, EventSubscription.

use cue::CueEvent;
use cue::types::{Model, TaskId, TaskOutcome, TaskPath, TaskPhase};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use tokio::sync::watch;
use traits::EventEmitter;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl From<CueEvent> for Event {
    fn from(event: CueEvent) -> Self {
        match event {
            CueEvent::TaskRegistered {
                task_id,
                parent_id,
                goal,
                depth,
            } => Self::TaskRegistered {
                task_id,
                parent_id,
                goal,
                depth,
            },
            CueEvent::PhaseTransition { task_id, phase } => {
                Self::PhaseTransition { task_id, phase }
            }
            CueEvent::PathSelected { task_id, path } => Self::PathSelected { task_id, path },
            CueEvent::ModelSelected { task_id, model } => Self::ModelSelected { task_id, model },
            CueEvent::SubtasksCreated {
                parent_id,
                child_ids,
            } => Self::SubtasksCreated {
                parent_id,
                child_ids,
            },
            CueEvent::TaskCompleted { task_id, outcome } => {
                Self::TaskCompleted { task_id, outcome }
            }
            CueEvent::TaskLimitReached { task_id } => Self::TaskLimitReached { task_id },
            CueEvent::BranchFixRound {
                task_id,
                round,
                model,
            } => Self::BranchFixRound {
                task_id,
                round,
                model,
            },
            CueEvent::FixSubtasksCreated {
                task_id,
                count,
                round,
            } => Self::FixSubtasksCreated {
                task_id,
                count,
                round,
            },
            CueEvent::RecoverySubtasksCreated {
                task_id,
                count,
                round,
            } => Self::RecoverySubtasksCreated {
                task_id,
                count,
                round,
            },
        }
    }
}

#[derive(Clone)]
pub struct EventLog {
    events: Arc<RwLock<Vec<Event>>>,
    notify_tx: Arc<watch::Sender<()>>,
}

pub struct EventSubscription {
    events: Arc<RwLock<Vec<Event>>>,
    offset: usize,
    notify_rx: watch::Receiver<()>,
}

impl EventLog {
    pub fn new() -> Self {
        let (notify_tx, _) = watch::channel(());
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
            notify_tx: Arc::new(notify_tx),
        }
    }

    /// Append an event. Returns the event's offset in the log.
    pub fn emit(&self, event: Event) -> usize {
        let mut events = self.events.write().unwrap();
        let offset = events.len();
        events.push(event);
        drop(events);
        let _ = self.notify_tx.send(());
        offset
    }

    /// Subscribe from offset 0 (full replay + live).
    pub fn subscribe(&self) -> EventSubscription {
        self.subscribe_from(0)
    }

    /// Subscribe from a specific offset.
    pub fn subscribe_from(&self, from: usize) -> EventSubscription {
        EventSubscription {
            events: Arc::clone(&self.events),
            offset: from,
            notify_rx: self.notify_tx.subscribe(),
        }
    }

    /// Current event count.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Whether the log is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.events.read().unwrap().is_empty()
    }

    /// Read-only snapshot of all events.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Vec<Event> {
        self.events.read().unwrap().clone()
    }
}

impl EventSubscription {
    /// Receive the next event. Blocks until one is available.
    ///
    /// `watch::Receiver::changed()` considers the initial channel value as
    /// unseen, so the first call returns immediately -- causing one harmless spin
    /// iteration when a subscription starts already caught up. The loop re-checks
    /// the Vec and blocks correctly on the second `changed()` call.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            {
                let events = self.events.read().unwrap();
                if self.offset < events.len() {
                    let event = events[self.offset].clone();
                    drop(events);
                    self.offset += 1;
                    return Some(event);
                }
            }
            if self.notify_rx.changed().await.is_err() {
                return None; // All senders dropped.
            }
        }
    }

    /// Non-blocking receive.
    #[allow(dead_code)]
    pub fn try_recv(&mut self) -> Option<Event> {
        let events = self.events.read().ok()?;
        if self.offset < events.len() {
            let event = events[self.offset].clone();
            drop(events);
            self.offset += 1;
            Some(event)
        } else {
            None
        }
    }
}

/// Bridge `CueEvent` into epic's `EventLog` by converting to `Event`.
/// `self.emit(Event::from(event))` calls the inherent `emit(Event)` method,
/// not the trait method, because inherent methods take precedence in Rust.
impl EventEmitter<CueEvent> for EventLog {
    fn emit(&self, event: CueEvent) {
        self.emit(Event::from(event));
    }
}
