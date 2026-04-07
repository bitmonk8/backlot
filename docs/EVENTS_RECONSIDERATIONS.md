# Epic Event System Reconsiderations

Research and analysis of epic's event system design, and implementation plan for the EventLog upgrade.

---

## Current Design

### Implementation

Epic's `src/events.rs`: 24 `Event` enum variants, `tokio::sync::mpsc::unbounded_channel`, fire-and-forget. One producer side (`EventSender`), one consumer side (`EventReceiver`).

Producers: epic's `orchestrator/mod.rs`, `task/leaf.rs`, `task/branch.rs` — emit via `EventSender` held in `Services<A>` (or directly by the orchestrator).

Consumer: either epic's TUI (`tui/mod.rs`) or a headless logger in `main.rs`. Never both simultaneously.

### Characteristics

| Property | Current |
|---|---|
| Channel type | `mpsc::unbounded_channel` (single consumer) |
| Persistence | None — events are ephemeral |
| Replay | Not possible |
| Backpressure | None (unbounded) |
| Consumer count | Exactly 1 |
| Serialization | Not derived (`Event` has no `Serialize`) |
| Ordering | FIFO within channel |
| Feedback into logic | None — purely observational |

### Functional Role

Events serve exactly two purposes:
1. **TUI rendering** — task tree updates, worklog entries, metrics panel
2. **Headless logging** — console output when TUI is disabled

Events do **not** drive state transitions, trigger side effects, or feed back into orchestration logic. The orchestrator and task methods use direct method calls and return values for all decision-making.

State persistence is a separate mechanism: periodic JSON snapshots of the full task tree to `.epic/state.json`.

---

## Limitations of Current Design

### L1: Single consumer

`mpsc` delivers each event to exactly one receiver. Adding a second consumer (e.g., JSONL file logger alongside TUI) requires either a fan-out task or switching to a multi-consumer primitive.

### L2: No persistence or replay

Events vanish after consumption. No audit trail, no post-run analysis from the event stream itself. JSONL logging (mentioned in EPIC_DESIGN.md) is not implemented — events are not serializable.

### L3: No late-join capability

A consumer that connects after events have been emitted (e.g., a web dashboard opened mid-run) sees nothing historical. It can only observe future events.

### L4: No backpressure

`unbounded_channel` buffers indefinitely. At epic's event volume (tens/second) this is not a practical concern, but it's architecturally unsound if a consumer stalls.

---

## Design Choice: EventLog

### Why Not Broadcast Channel First?

A `tokio::sync::broadcast` channel was considered as an incremental step (multi-consumer, minimal code change). Rejected because:

- EventLog is only ~70 lines more code than a broadcast migration
- EventLog subsumes all broadcast capabilities (multi-consumer) and adds replay, late-join, and persistence
- A broadcast-first approach means migrating send/receive sites twice
- EventLog uses a simpler consumer API (`Option<Event>` vs `Result<Event, TryRecvError>`)

### EventLog Design

An append-only in-memory Vec with a `watch` channel for change notification. Consumers subscribe with an offset and receive all events from that point forward.

```
EventLog
  ├── emit(event)              // push to Vec + notify
  ├── subscribe()              // returns subscription from offset 0
  ├── subscribe_from(offset)   // returns subscription from specific offset
  ├── len()                    // current event count
  └── snapshot()               // read-only copy of all events
```

**Properties gained:**

| Property | EventLog |
|---|---|
| Multi-consumer | Yes — each subscriber independently tracks its offset |
| Late-join replay | Yes — subscribe from offset 0 gets full history |
| Persistence | Optional — JSONL logger is just another subscriber |
| Audit trail | Yes — `snapshot()` returns all events after run completes |
| Backpressure | N/A — Vec grows, consumers read at their own pace |
| Consumer API | `Option<Event>` (simpler than channel `Result` types) |

**Notification mechanism:** `tokio::sync::watch<usize>` carries the current event count. Consumers compare their offset to the watched value. When they've caught up, they wait on `watch::changed()`. No polling.

---

## Implementation Guide

### Step 1: Add Serialize/Deserialize to Event

Add derives to `Event` and its contained types (`TaskId`, `TaskPhase`, `TaskPath`, `Model`, `TaskOutcome`).

```rust
// events.rs
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    // ... all 24 variants unchanged
}
```

Most contained types already derive these for state persistence; add any missing derives. No behavioral change.

### Step 2: Implement EventLog and EventSubscription

```rust
// events.rs
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

#[derive(Clone)]
pub struct EventLog {
    events: Arc<RwLock<Vec<Event>>>,
    len_tx: Arc<watch::Sender<usize>>,
}

pub struct EventSubscription {
    events: Arc<RwLock<Vec<Event>>>,
    offset: usize,
    len_rx: watch::Receiver<usize>,
}

impl EventLog {
    pub fn new() -> Self {
        let (len_tx, _) = watch::channel(0usize);
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
            len_tx: Arc::new(len_tx),
        }
    }

    /// Append an event. Returns the event's offset in the log.
    pub fn emit(&self, event: Event) -> usize {
        let mut events = self.events.write().unwrap();
        let offset = events.len();
        events.push(event);
        drop(events);
        let _ = self.len_tx.send(offset + 1);
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
            len_rx: self.len_tx.subscribe(),
        }
    }

    /// Current event count.
    pub fn len(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Read-only snapshot of all events.
    pub fn snapshot(&self) -> Vec<Event> {
        self.events.read().unwrap().clone()
    }
}

impl EventSubscription {
    /// Receive the next event. Blocks until one is available.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            {
                let events = self.events.read().unwrap();
                if self.offset < events.len() {
                    let event = events[self.offset].clone();
                    self.offset += 1;
                    return Some(event);
                }
            }
            if self.len_rx.changed().await.is_err() {
                return None; // All senders dropped.
            }
        }
    }

    /// Non-blocking receive.
    pub fn try_recv(&mut self) -> Option<Event> {
        let events = self.events.read().ok()?;
        if self.offset < events.len() {
            let event = events[self.offset].clone();
            self.offset += 1;
            Some(event)
        } else {
            None
        }
    }
}
```

Key design notes:
- **`std::sync::RwLock`**, not `tokio::sync::RwLock`. The write lock is held for ~1us (Vec push). No async needed for producers. This keeps `emit()` synchronous, matching the current `let _ = sender.send(event)` pattern.
- **`emit()` takes ownership** of the event. No clone needed on the producer side.
- **`Event` needs `Clone`** only for the subscription side (`recv`/`try_recv` clone from the Vec). This is already derived on `Event`.
- **`try_recv` uses `read().ok()?`** — returns `None` if the lock is poisoned or contended, which is the correct non-blocking behavior.

### Step 3: Update send sites

All send sites currently use `let _ = sender.send(event)`. Change to `let _ = log.emit(event)`.

This is a mechanical rename. `emit()` is sync and infallible (panics only on poisoned lock, which indicates a bug). The `let _ =` pattern drops the returned offset, matching the current fire-and-forget semantics.

### Step 4: Update receive sites

**TUI** (`tui/mod.rs`): Currently calls `event_rx.try_recv()` in a polling loop.

```rust
// Before (mpsc)
match event_rx.try_recv() {
    Ok(event) => self.handle_event(event),
    Err(mpsc::error::TryRecvError::Empty) => {},
    Err(mpsc::error::TryRecvError::Disconnected) => { self.orchestrator_done = true; }
}

// After (EventSubscription)
if let Some(event) = subscription.try_recv() {
    self.handle_event(event);
}
// Disconnection detected when EventLog is dropped and try_recv returns None
// after all events are consumed. TUI already handles this via orchestrator
// completion signals.
```

**Headless logger** (`main.rs`): Same pattern.

The consumer API is simpler: `Option<Event>` instead of `Result<Event, TryRecvError>`.

### Step 5: Enable multiple consumers

```rust
// main.rs
let log = EventLog::new();

let tui_sub = log.subscribe();      // TUI consumer — replays from 0
let logger_sub = log.subscribe();   // JSONL file logger — replays from 0

// Pass log.clone() to Orchestrator (producer)
// Pass tui_sub to TUI
// Spawn logger task with logger_sub
```

Example JSONL logger consumer:

```rust
async fn jsonl_logger(mut sub: EventSubscription, path: PathBuf) {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true).append(true).open(&path).await.unwrap();
    while let Some(event) = sub.recv().await {
        let line = serde_json::to_string(&event).unwrap();
        file.write_all(line.as_bytes()).await.unwrap();
        file.write_all(b"\n").await.unwrap();
    }
}
```

### Step 6: Update tests

Tests that create `event_channel()` and assert on received events need updated to use `EventLog::new()` + `subscribe()`. Tests that discard the receiver need no changes — `EventLog` with no subscribers works fine (events accumulate in the Vec).

### Step 7: Post-run analysis

After the orchestrator completes, `log.snapshot()` returns all events for summary output, JSONL persistence to `.epic/events.jsonl`, or aggregate metric computation. This replaces the current pattern where events vanish after consumption.

---

## Cross-Crate Event Propagation

### Current Crate Hierarchy

```
epic (application)
  ├── cue (orchestrator framework)
  ├── reel (agent session layer)
  ├── vault (document store)
  ├── lot (process sandboxing)
  └── flick (model call layer, used by reel)
```

Events propagate **upward**: lower crates emit, higher crates consume. No crate should depend on a sibling's event types. No globals.

### Injected EventLog

Every crate that emits events receives an `EventLog` handle at construction time. The `EventLog` is `Clone` (all fields are `Arc`-wrapped). Cloning gives another handle to the same log.

```
epic (creates EventLog, distributes subscriptions to consumers)
  │
  ├── let log = EventLog::new();
  │
  ├── cue::Orchestrator receives log.clone() ──── (via constructor)
  │     └── passes log.clone() to TaskRuntime ──── (via create_subtask / bind_runtime)
  │           └── Task methods call log.emit()
  │
  ├── tui_sub = log.subscribe();        ◄──── TUI replays from 0
  ├── logger_sub = log.subscribe();     ◄──── JSONL logger replays from 0
  └── web_sub = log.subscribe_from(n);  ◄──── Late-joining dashboard replays from offset n
```

### Why Not a Trait for the Sink?

A trait (`trait EventSink { fn emit(&self, event: Event); }`) adds indirection for no practical benefit. The event log is always the same concrete type. Using the concrete type:
- Enables cheap `Clone` (Arc refcount)
- Avoids `dyn` / boxing overhead
- Keeps the API to one type and one method

### After Extraction to Cue

`EventLog`, `EventSubscription`, and the `Event` enum move to cue. Epic creates the log, passes it to the orchestrator, and distributes subscriptions to consumers.

```rust
// In cue crate
pub struct Orchestrator<S: TaskStore> {
    store: S,
    events: EventLog,
    limits: LimitsConfig,
    state_path: Option<PathBuf>,
}
```

### Future Crate Event Propagation

Reel and vault emit no events today. If they grow to need mid-execution notifications:

**Preferred: crate-local event enum + mapping.** The lower crate defines its own event enum (e.g., `reel::AgentEvent`) and accepts a callback or channel. Epic maps these into `cue::Event` and emits into the main EventLog. No dependency from reel to cue.

```
epic
  ├── main EventLog (cue::Event)
  │     ▲
  │     │ mapping: reel::AgentEvent → cue::Event → log.emit()
  │     │
  └── reel emits its own AgentEvent type
```

This preserves crate independence. Reel should not depend on cue.

---

## Migration Summary

| Step | What changes | Lines changed (est.) | Risk |
|---|---|---|---|
| 1 | Add `Serialize`/`Deserialize` derives | ~10 | None |
| 2 | Add EventLog + EventSubscription | ~70 new | Low |
| 3 | Change send sites (`send` → `emit`) | ~40 (rename) | Low |
| 4 | Change consumer receive calls | ~20 | Low |
| 5 | Enable second consumer (JSONL logger) | ~30 new | Low |
| 6 | Update test event assertions | ~30 | Low |
| 7 | Post-run snapshot usage | ~10 new | Low |
| **Total** | | **~210** | **Low** |

### Ordering Relative to Orchestrator Extraction

The EventLog belongs in cue. Building it in epic first then moving it during extraction is viable but creates unnecessary churn. **Recommended: implement as part of or after extraction**, so EventLog lands in cue from the start.

The extraction's preparatory phases (decision collapsing, cross-task queries, type decoupling, trait definitions, boundary verification) do not touch the event channel type. No conflict.

---

## Research Sources

- Tokio channel documentation (mpsc, broadcast, watch semantics)
- Rust community patterns for event-driven systems (users.rust-lang.org)
- Production orchestrator architectures (Temporal, Prefect, Airflow event sourcing patterns)
- Implementation pattern comparison: Vec+watch vs Vec+Notify vs broadcast+Vec hybrid
