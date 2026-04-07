# Epic Event System Reconsiderations

Research and analysis of epic's event system design, alternatives, and recommendations for future extensibility.

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

`mpsc` delivers each event to exactly one receiver. Adding a second consumer (e.g., JSONL file logger alongside TUI) requires either:
- A fan-out task that receives from mpsc and forwards to per-consumer channels
- Switching to a multi-consumer primitive

### L2: No persistence or replay

Events vanish after consumption. No audit trail, no post-run analysis from the event stream itself. JSONL logging (mentioned in EPIC_DESIGN.md) is not implemented — events are not serializable.

### L3: No late-join capability

A consumer that connects after events have been emitted (e.g., a web dashboard opened mid-run) sees nothing historical. It can only observe future events.

### L4: Dual state mechanisms

State lives in two places: the event stream (ephemeral) and `state.json` (persisted snapshots). These are not connected — events cannot reconstruct state, and state snapshots don't capture the event history.

### L5: No backpressure

`unbounded_channel` buffers indefinitely. At epic's event volume (tens/second) this is not a practical concern, but it's architecturally unsound if a consumer stalls (e.g., network-bound Slack webhook).

### L6: No subscriber filtering

Every consumer receives every event. A metrics aggregator that only cares about `UsageUpdated` still receives all 24 event types.

---

## Research: Rust Ecosystem Patterns

### Channel Primitives

| Primitive | Semantics | Replay | Consumers | Backpressure |
|---|---|---|---|---|
| `tokio::sync::mpsc` | Multi-producer, single-consumer | No | 1 | Bounded variant only |
| `tokio::sync::broadcast` | Multi-producer, multi-consumer | No (late joiners miss history) | N | Sender blocks if all receivers lag |
| `tokio::sync::watch` | Single value, multi-consumer | Latest value only | N | No (latest wins) |
| `crossbeam-channel` | Sync MPMC | No | N | Bounded variant only |

**Key finding**: No standard Rust channel primitive supports replay from offset. Replay requires a separate log/buffer.

### Common Application Patterns

1. **Broadcast channel** — simplest multi-consumer upgrade. Each subscriber gets all future events. No replay, no persistence. Suitable when all consumers are present at startup.

2. **Event log + notify** — `Arc<RwLock<Vec<Event>>>` with `tokio::sync::Notify`. Append-only in-memory log. Consumers track their own offset. Supports late-join replay. Optional JSONL persistence via append-mode file writes.

3. **Event bus with typed subscriptions** — consumers register handlers for specific event types. Reduces noise. More boilerplate.

### What Production Orchestrators Use

Temporal, Prefect, Airflow, and similar workflow engines converge on:
- **Event sourcing or transaction log** as the primary state mechanism for durability and replay
- **Fan-out** (pub/sub, materialized views) for multiple consumers
- **OpenTelemetry** as a separate observability layer

However, these are distributed systems with databases. Epic is a single-process application. The complexity gap is significant.

---

## Alternatives Analysis

### Option A: Upgrade to `broadcast` Channel

Replace `mpsc::unbounded_channel` with `tokio::sync::broadcast::channel(capacity)`.

**Pros**:
- Minimal code change (channel creation + receiver cloning)
- Native multi-consumer: TUI, file logger, webhook forwarder each get their own receiver
- Bounded: provides backpressure (lagging consumers see `RecvError::Lagged`)

**Cons**:
- No replay — late joiners miss history
- No persistence built-in
- Lagged consumers lose events (must handle `Lagged` error)
- Event must be `Clone` (already is)

**Implementation cost**: Low. Change channel creation, clone receivers per consumer, handle `Lagged`.

**Verdict**: Good incremental step. Unblocks multi-consumer. Does not address persistence or replay.

### Option B: In-Memory Event Log with Subscribers

Custom `EventLog` struct: `Arc<RwLock<Vec<Event>>>` + `watch<usize>` notification. Consumers subscribe with an offset and receive all events from that point forward.

```
EventLog
  ├── append(event)          // push + notify + optional JSONL write
  ├── subscribe(from_offset) // returns stream of events from offset
  ├── get(offset)            // random access
  └── len()                  // current log length
```

**Pros**:
- Multi-consumer with replay from any offset
- Late-join capable (web dashboard opened mid-run gets full history)
- Optional JSONL persistence (append-mode file writes)
- Events become the audit trail
- Low complexity — no external dependencies, simple Vec-based storage

**Cons**:
- More code than broadcast (~70 lines for the EventLog)
- Memory grows linearly (acceptable at epic's volume: 24 event types, ~hundreds of events per run)
- Need to handle serialization (add `Serialize`/`Deserialize` to `Event`)
- Consumers must manage their own cursors

**Implementation cost**: Moderate. New struct, serialization derives, consumer cursor management.

**Verdict**: Addresses multi-consumer, replay, late-join, and persistence in one mechanism. Proportionate complexity for the value delivered.

### Is Option B Overkill?

The honest counterargument: epic's consumers (TUI, file logger) all start at process startup. They never join late. A web dashboard, if built, would likely need a task tree snapshot + live updates anyway — not event replay. The replay capability that makes Option B interesting may never be exercised.

The pragmatic path: **broadcast (Option A) handles the known requirements**. Option B's replay is speculative. If a late-joining consumer eventually needs history, the cheaper solution is: give it a state snapshot from `EpicState`, then subscribe to broadcast for live updates. This avoids building replay infrastructure for a scenario that may not materialize.

**When Option B stops being overkill**:
- Post-run analysis from the event stream (not just state snapshots)
- JSONL audit trail as a first-class feature
- A consumer that genuinely reconstructs its view from event history (not state snapshots)

---

## Comparison Matrix

| Criterion | A: Broadcast | B: Event Log |
|---|---|---|
| Multi-consumer | Yes | Yes |
| Late-join replay | No | Yes |
| Persistence | No | Optional JSONL |
| Audit trail | No | Yes (if persisted) |
| Unifies state persistence | No | No |
| Implementation cost | Low | Moderate |
| Complexity added | Minimal | Low |
| External dependencies | None | None |
| Extraction impact (cue) | Minimal | EventLog moves to cue |

---

## Interaction with Orchestrator Extraction

The extraction spec ([ORCHESTRATOR_EXTRACTION.md](ORCHESTRATOR_EXTRACTION.md)) plans to move `Event`, `EventSender`, `EventReceiver`, and `event_channel()` into the cue crate. The orchestrator holds an `EventSender` directly.

**Both options are compatible** with this plan — the channel/log type moves to cue, consumers live in epic. The interface boundary (`EventSender` or `EventLog` handle) stays clean.

---

## Recommendations

### Near-term (before or during extraction): Option A — Broadcast Channel

Switch from `mpsc::unbounded_channel` to `tokio::sync::broadcast::channel`. This is a small, low-risk change that unblocks multi-consumer scenarios.

Specific steps:
1. Add `Serialize`, `Deserialize` derives to `Event` (prepares for future persistence regardless of chosen path)
2. Replace `mpsc` with `broadcast`
3. Each consumer (TUI, headless logger) subscribes independently

This change is compatible with the extraction plan — `broadcast` channel types move to cue identically to the current `mpsc` types.

### Medium-term (post-extraction, when a second UI backend is being built): Option B — Event Log

When the first non-TUI consumer is actually needed (web dashboard, Slack integration), upgrade from broadcast to an EventLog that provides replay and optional JSONL persistence.

The EventLog can wrap a broadcast channel internally for real-time delivery while maintaining an in-memory Vec for replay. This is an additive change — existing consumers continue to work.

### Immediate low-cost preparation (regardless of path)

Add `Serialize` and `Deserialize` to `Event` now. This is a one-line derive change with zero behavioral impact. Every future option benefits from serializable events, and it enables JSONL file logging immediately.

---

## Cross-Crate Event Propagation Design

### Current Crate Hierarchy

```
epic (application)
  ├── cue (orchestrator framework, to be extracted)
  ├── reel (agent session layer)
  ├── vault (document store)
  ├── lot (process sandboxing)
  └── flick (model call layer, used by reel)
```

Events propagate **upward**: lower crates emit, higher crates consume. No crate should depend on a sibling's event types. No globals.

### Current Event Emission Sites

Two categories of emitters today:

1. **Orchestrator coordinator** (`orchestrator/mod.rs`) — emits via `self.services.events.send()`:
   `TaskRegistered`, `PhaseTransition`, `PathSelected`, `ModelSelected`, `SubtasksCreated`, `TaskCompleted`, `BranchFixRound`, `FixSubtasksCreated`, `RecoverySubtasksCreated`, `TaskLimitReached`, `UsageUpdated`, `FileLevelReviewCompleted`, `VaultRecorded`, `VaultReorganizeCompleted`

2. **Task lifecycle methods** (`task/leaf.rs`, `task/branch.rs`) — emit via `svc.events.send()`:
   `RetryAttempt`, `FixAttempt`, `FixModelEscalated`, `ModelEscalated`, `DiscoveriesRecorded`, `FileLevelReviewCompleted`, `UsageUpdated`, `VaultRecorded`, `RecoveryStarted`, `RecoveryPlanSelected`, `CheckpointAdjust`, `CheckpointEscalate`

### Sibling Crate Status

**Reel and vault emit no events today.** Both are return-value-only: they complete an operation and return a `RunResult<T>` or `SessionMetadata`. No mid-execution notifications. No callback traits, no channels, no observer patterns.

If these crates grow to need mid-execution notifications (e.g., reel emitting `ToolExecuted` during an agent turn, vault emitting `DocumentCreated` during a record operation), the same sink-injection pattern described below applies.

### Design Principle: Injected Event Sinks

Every crate that emits events receives an event sink at construction time. The sink is a concrete channel sender type, not a trait object. The crate that *creates* the channel owns the receiver(s). Events flow upward through the dependency graph via the sender.

```
epic (owns channel, holds receiver, spawns consumers)
  │
  ├── creates broadcast::Sender<Event>  ──┐
  │                                        │
  ├── cue::Orchestrator receives sender ───┤  (via constructor arg)
  │     └── passes to TaskNode impls       │  (via TaskRuntime / Services)
  │                                        │
  └── TUI / logger / webhook subscribe  ◄──┘  (via broadcast::Receiver<Event>)
```

After extraction, cue defines the `Event` enum and the sender/receiver type aliases. Epic creates the channel, passes the sender down, keeps receiver(s) for consumers.

### Why Not a Trait for the Sink?

A trait (`trait EventSink { fn emit(&self, event: Event); }`) adds indirection and dynamic dispatch for no practical benefit. The event sender is always the same concrete type (`broadcast::Sender<Event>` or `EventLog` handle). Using the concrete type:
- Enables `Clone` (broadcast senders are cheaply cloneable)
- Avoids `dyn` / boxing overhead
- Keeps the API simple — one type, one `send()` method
- Matches the existing pattern (current code uses `EventSender` type alias directly)

If a future crate needs to emit its own domain-specific events (e.g., reel emitting `ReelEvent`), it defines its own event enum and its own sender type. The consuming crate (epic) maps those into `Event` at the boundary. No shared event trait needed.

---

## Implementation Guide: A then B

### Phase A: Upgrade to Broadcast

#### A.1: Add Serialize/Deserialize to Event

Add derives to `Event` and its contained types (`TaskId`, `TaskPhase`, `TaskPath`, `Model`, `TaskOutcome`).

```rust
// events.rs
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    // ... all 24 variants unchanged
}
```

Check that all types referenced by Event variants already derive `Serialize`/`Deserialize`. Types to verify: `TaskId`, `TaskPhase`, `TaskPath`, `Model`, `TaskOutcome`. Most already derive these for state persistence; add any missing derives.

No behavioral change. Tests should pass unchanged.

#### A.2: Replace mpsc with broadcast

```rust
// events.rs — before
pub type EventSender = mpsc::UnboundedSender<Event>;
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

pub fn event_channel() -> (EventSender, EventReceiver) {
    mpsc::unbounded_channel()
}

// events.rs — after
use tokio::sync::broadcast;

pub type EventSender = broadcast::Sender<Event>;
pub type EventReceiver = broadcast::Receiver<Event>;

const EVENT_CHANNEL_CAPACITY: usize = 1024;

pub fn event_channel() -> (EventSender, EventReceiver) {
    let (tx, rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    (tx, rx)
}
```

Capacity 1024 is generous for epic's volume (hundreds of events per run). `RecvError::Lagged` is the backpressure signal if a consumer falls behind.

#### A.3: Update send sites

`broadcast::Sender::send()` returns `Result<usize, SendError<T>>` (number of receivers that got the message). Current code already discards the result with `let _ = ...send()`. No change needed at send sites.

One difference: `broadcast::send()` fails if there are **zero receivers** (returns `SendError`). Current code already ignores errors. Verify this is acceptable — it means events emitted before any consumer subscribes are silently dropped. This matches the current mpsc behavior (events are consumed, not accumulated).

#### A.4: Update receive sites

`broadcast::Receiver::recv()` is async and returns `Result<Event, RecvError>`. `RecvError::Lagged(n)` means the consumer missed `n` events (buffer wrapped). `RecvError::Closed` means all senders dropped.

**TUI** (`tui/mod.rs`): Currently calls `event_rx.try_recv()` in a polling loop. Update to handle `RecvError::Lagged`:

```rust
// Before (mpsc)
match event_rx.try_recv() {
    Ok(event) => self.handle_event(event),
    Err(mpsc::error::TryRecvError::Empty) => {},
    Err(mpsc::error::TryRecvError::Disconnected) => { self.orchestrator_done = true; }
}

// After (broadcast)
match event_rx.try_recv() {
    Ok(event) => self.handle_event(event),
    Err(broadcast::error::TryRecvError::Empty) => {},
    Err(broadcast::error::TryRecvError::Closed) => { self.orchestrator_done = true; }
    Err(broadcast::error::TryRecvError::Lagged(_)) => {
        // Consumer fell behind — events lost. Continue with next available.
        // At epic's event volume this should not happen with capacity 1024.
    }
}
```

**Headless logger** (`main.rs`): Same pattern.

#### A.5: Enable multiple consumers

With broadcast, additional consumers subscribe by calling `tx.subscribe()`:

```rust
// main.rs
let (tx, _) = event_channel();  // broadcast returns (Sender, Receiver)
                                 // first receiver is unused — subscribe explicitly

let tui_rx = tx.subscribe();     // TUI consumer
let log_rx = tx.subscribe();     // JSONL file logger (new)

// Pass tx to Orchestrator (sender)
// Pass tui_rx to TUI
// Spawn logger task with log_rx
```

Note: `broadcast::channel()` returns one receiver, but it's cleaner to discard it and create all receivers via `subscribe()` so the pattern is uniform.

#### A.6: Update tests

Tests that create `event_channel()` and check received events need updated receive calls. Tests that discard the receiver (`_rx`) need no changes — broadcast senders work fine with no active receivers (send returns `Err` which is already ignored).

For tests that assert on received events: `broadcast::Receiver::try_recv()` returns `Result<T, TryRecvError>` vs mpsc's. Update match arms.

#### A.7: Extraction boundary (cue crate)

After extraction, `events.rs` moves to cue. The type aliases and `event_channel()` live in cue. Epic creates the channel, passes `EventSender` into `cue::Orchestrator`, and keeps/distributes `EventReceiver`s to consumers.

```
cue crate:
  - defines Event enum, EventSender, EventReceiver, event_channel()
  - Orchestrator<S: TaskStore> holds EventSender
  - TaskNode impls receive EventSender via runtime injection

epic crate:
  - calls cue::event_channel()
  - passes sender to Orchestrator and TaskRuntime
  - subscribes receivers for TUI, logger, webhooks
```

The `EventSender` (`broadcast::Sender<Event>`) is `Clone`. Cloning it is cheap and gives another sender handle — multiple producers can send into the same channel. This is how task lifecycle methods get their own sender: via `Clone` on the sender held in `Services`/`TaskRuntime`.

---

### Phase B: Upgrade to EventLog

Upgrade from broadcast to an EventLog when evidence demands replay or persistence. The EventLog wraps broadcast internally — existing consumer code changes minimally.

#### B.1: EventLog struct

```rust
// events.rs (in cue crate)
use std::sync::Arc;
use tokio::sync::{broadcast, watch, RwLock};
use serde::{Serialize, Deserialize};

#[derive(Clone)]
pub struct EventLog {
    events: Arc<RwLock<Vec<Event>>>,
    len_tx: Arc<watch::Sender<usize>>,
    broadcast_tx: broadcast::Sender<Event>,
}

pub struct EventSubscription {
    events: Arc<RwLock<Vec<Event>>>,
    offset: usize,
    len_rx: watch::Receiver<usize>,
}

const BROADCAST_CAPACITY: usize = 1024;

impl EventLog {
    pub fn new() -> Self {
        let (broadcast_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (len_tx, _) = watch::channel(0usize);
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
            len_tx: Arc::new(len_tx),
            broadcast_tx,
        }
    }

    /// Append an event. Returns the event's offset in the log.
    pub async fn emit(&self, event: Event) -> usize {
        let mut events = self.events.write().await;
        let offset = events.len();
        events.push(event.clone());
        drop(events);
        let _ = self.len_tx.send(offset + 1);
        let _ = self.broadcast_tx.send(event);
        offset
    }

    /// Subscribe from offset 0 (full replay + live).
    pub fn subscribe(&self) -> EventSubscription {
        self.subscribe_from(0)
    }

    /// Subscribe from a specific offset. Events before `from` are skipped.
    pub fn subscribe_from(&self, from: usize) -> EventSubscription {
        EventSubscription {
            events: Arc::clone(&self.events),
            offset: from,
            len_rx: self.len_tx.subscribe(),
        }
    }

    /// Current event count.
    pub async fn len(&self) -> usize {
        self.events.read().await.len()
    }

    /// Read-only snapshot of all events (for persistence, post-run analysis).
    pub async fn snapshot(&self) -> Vec<Event> {
        self.events.read().await.clone()
    }
}

impl EventSubscription {
    /// Receive the next event. Blocks until one is available.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            // Check if there are unread events.
            {
                let events = self.events.read().await;
                if self.offset < events.len() {
                    let event = events[self.offset].clone();
                    self.offset += 1;
                    return Some(event);
                }
            }
            // Wait for new events.
            if self.len_rx.changed().await.is_err() {
                return None; // All senders dropped.
            }
        }
    }

    /// Non-blocking receive. Returns None if no new events.
    pub fn try_recv(&mut self) -> Option<Event> {
        // Can't async-read here; use try_lock for non-blocking path.
        let events = self.events.try_read().ok()?;
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

~70 lines. Single source of truth (Vec). Broadcast kept internally for compatibility but not required — consumers use `EventSubscription`.

#### B.2: Producer API change

The producer API changes from `sender.send(event)` to `log.emit(event).await`.

**Problem**: `emit()` is async (takes write lock). Current send sites use synchronous `let _ = tx.send(event)`. Two options:

**(a) Make emit sync** — Use `std::sync::RwLock` instead of `tokio::sync::RwLock`. Write lock is held for ~1us (Vec push). No async required. This is the better choice for epic's single-producer, low-contention scenario.

**(b) Keep emit async** — Requires `.await` at every send site. Adds noise. Only justified if write contention is a concern (it isn't at epic's volume).

**Recommendation**: Use `std::sync::RwLock` for the Vec. Keep `tokio::sync::watch` for notification (watch is async-native and used only on the consumer side). The `emit()` method becomes sync:

```rust
pub fn emit(&self, event: Event) -> usize {
    let mut events = self.events.write().unwrap();
    let offset = events.len();
    events.push(event.clone());
    drop(events);
    let _ = self.len_tx.send(offset + 1);
    let _ = self.broadcast_tx.send(event);
    offset
}
```

This preserves the current `let _ = log.emit(event)` call pattern — no `.await` needed at send sites.

#### B.3: Consumer API change

**TUI**: Replace `event_rx.try_recv()` with `subscription.try_recv()`. Same polling pattern. `try_recv()` returns `Option<Event>` instead of `Result`. Simpler.

**Headless logger**: Same change.

**New JSONL logger** (example consumer):

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

#### B.4: Cross-crate propagation with EventLog

The `EventLog` is `Clone` (all fields are `Arc`-wrapped or `Clone`). Cloning gives another handle to the same log. This replaces the broadcast sender's `Clone` semantics.

```
epic (creates EventLog, distributes subscriptions to consumers)
  │
  ├── let log = EventLog::new();
  │
  ├── cue::Orchestrator receives log.clone() ──── (via constructor)
  │     └── passes log.clone() to TaskRuntime ──── (via create_subtask / bind_runtime)
  │           └── Task methods call log.emit()
  │
  ├── tui_sub = log.subscribe();        ◄──── TUI gets subscription from offset 0
  ├── logger_sub = log.subscribe();     ◄──── JSONL logger gets subscription from offset 0
  └── web_sub = log.subscribe_from(n);  ◄──── Late-joining web dashboard replays from offset n
```

**Type alias update** (in cue crate):

```rust
// Phase A (broadcast)
pub type EventSender = broadcast::Sender<Event>;

// Phase B (EventLog) — EventSender is replaced by EventLog
// The Orchestrator and TaskRuntime hold EventLog instead of EventSender.
// Consumers hold EventSubscription instead of EventReceiver.
```

The extraction spec's `Orchestrator` struct changes:

```rust
// Phase A
pub struct Orchestrator<S: TaskStore> {
    store: S,
    events: EventSender,  // broadcast::Sender<Event>
    limits: LimitsConfig,
    state_path: Option<PathBuf>,
}

// Phase B
pub struct Orchestrator<S: TaskStore> {
    store: S,
    events: EventLog,  // replaces EventSender
    limits: LimitsConfig,
    state_path: Option<PathBuf>,
}
```

The call sites change from `events.send(event)` to `events.emit(event)` — a rename, not a structural change.

#### B.5: Future crate event propagation

If reel or vault grow to emit their own events mid-execution:

**Option 1: Injected EventLog handle** — The lower crate receives an `EventLog` (or a sender handle) at construction. It emits events directly into the shared log. This requires the lower crate to depend on cue for the `Event` type.

**Option 2: Crate-local event enum + mapping** — The lower crate defines its own event enum (e.g., `reel::AgentEvent`) and accepts a `broadcast::Sender<reel::AgentEvent>`. Epic creates a separate channel for reel events, spawns a mapping task that converts `reel::AgentEvent` into `cue::Event`, and emits into the main EventLog. No dependency from reel to cue.

```
epic
  ├── main EventLog (cue::Event)
  │     ▲
  │     │ mapping task: reel::AgentEvent → cue::Event → log.emit()
  │     │
  └── reel event channel (reel::AgentEvent)
        ▲
        │
      reel::Agent emits into its own channel
```

**Option 2 is preferred** — it preserves crate independence. Reel should not depend on cue. The mapping lives in epic, which depends on both.

**Option 3: Callback trait** — The lower crate defines a trait (`trait AgentObserver { fn on_tool_executed(...); }`). Epic implements it, mapping calls to EventLog emits. More boilerplate than Option 2 but avoids any channel in the lower crate.

For the current state (reel and vault are return-value-only), none of this is needed. The design accommodates it when the time comes.

#### B.6: Persistence (optional JSONL sink)

EventLog can optionally write every event to a JSONL file. Two approaches:

**(a) Built-in**: EventLog takes an optional file path at construction. `emit()` appends a JSON line after pushing to the Vec.

**(b) External consumer**: A dedicated JSONL logger subscribes at offset 0 and writes to file. No persistence logic in EventLog itself.

**Recommendation**: **(b)**. Keeps EventLog simple. Persistence is just another consumer. The JSONL logger consumer shown in B.3 is ~10 lines.

#### B.7: Post-run analysis

After the orchestrator completes, `log.snapshot()` returns all events. Epic can:
- Write a summary to stdout (headless mode)
- Persist the full event log to `.epic/events.jsonl` for post-run tooling
- Compute aggregate metrics from the event history

This is a capability that broadcast alone cannot provide — the events are gone after consumption.

---

### Migration Summary

| Step | What changes | Lines changed (est.) | Risk |
|---|---|---|---|
| A.1 | Add `Serialize`/`Deserialize` derives | ~10 | None |
| A.2 | Replace mpsc with broadcast in `events.rs` | ~10 | Low |
| A.3 | Verify send sites (no change needed) | 0 | None |
| A.4 | Update TUI + headless receive loops | ~20 | Low |
| A.5 | Enable second consumer (JSONL logger) | ~30 new | Low |
| A.6 | Update test event assertions | ~30 | Low |
| A.7 | Move to cue during extraction | Mechanical | Low |
| **A total** | | **~100** | **Low** |
| B.1 | Add EventLog + EventSubscription | ~70 new | Low |
| B.2 | Change emit call sites | ~40 (rename) | Low |
| B.3 | Change consumer receive calls | ~20 | Low |
| B.4 | Update Orchestrator/TaskRuntime types | ~10 | Low |
| **B total (incremental over A)** | | **~140** | **Low** |

Both phases are backward-compatible with the extraction plan. A is a prerequisite for B only in the sense that B includes broadcast internally — you could skip A and go directly to B if the timing aligns.

---

### Ordering Relative to Orchestrator Extraction

#### Phase A (broadcast): Orthogonal to extraction

The mpsc→broadcast change is behind the `EventSender`/`EventReceiver` type aliases. The extraction moves those aliases regardless of what they point to. Send sites use the same `let _ = sender.send(event)` pattern either way. Three viable orderings:

1. **A before extraction** — Change in epic, then mechanically move broadcast-based `events.rs` to cue.
2. **A during extraction** — Bundle the switch into Phase 6 (mechanical extraction) since `events.rs` is being moved anyway. Avoids touching the file twice.
3. **A after extraction** — Change in cue, update receive sites in epic. Works but touches two crates.

No ordering creates complications or rework. **Recommended: bundle with extraction (Phase 6).**

#### Phase B (EventLog): Slightly cleaner after extraction

The `EventLog` struct belongs in cue. `EventSubscription` consumers belong in epic. Doing B after extraction means the crate boundary enforces this separation — the struct lands in the right crate from the start. Doing B before extraction means building it in epic then moving it — unnecessary churn.

The extraction's preparatory phases (1-5: decision collapsing, cross-task queries, type decoupling, trait definitions, boundary verification) do not touch the event channel type. No conflict.

**Recommended: B after extraction, when evidence demands it.**

---

## Research Sources

- Tokio channel documentation (mpsc, broadcast, watch semantics)
- Rust community patterns for event-driven systems (users.rust-lang.org)
- Production orchestrator architectures (Temporal, Prefect, Airflow event sourcing patterns)
- Event sourcing / CQRS patterns in Rust (oneuptime.com, Microsoft Azure docs)
- Multi-backend event distribution best practices (industry patterns for UI + webhook + metrics fan-out)
- Crate ecosystem survey: eventfold, eventlogs, armature-cqrs, crates.io keyword searches (append-only, event-log, event-store, event-stream)
- Implementation pattern comparison: Vec+watch vs Vec+Notify vs broadcast+Vec hybrid
