# Epic Event System Reconsiderations

Research and analysis of epic's event system design, and implementation plan for the EventLog upgrade.

---

## Current Design

### Implementation

Cue's `src/events.rs`: 24 `Event` enum variants, `tokio::sync::mpsc::unbounded_channel`, fire-and-forget. One producer side (`EventSender`), one consumer side (`EventReceiver`). Epic re-exports these types via `pub use cue::{Event, EventReceiver, EventSender, event_channel}`.

Producers: cue's `orchestrator.rs` emits via `EventSender` held directly on the `Orchestrator` struct. Epic's `task/node_impl.rs` emits via `EventSender` held in `TaskRuntime<A>`.

Consumer: epic's TUI (`tui/mod.rs`) when enabled, otherwise the receiver is dropped (no headless logger exists today).

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

Events serve one purpose today:
1. **TUI rendering** — task tree updates, worklog entries, metrics panel

In headless mode (`--no-tui`), the receiver is dropped and events are discarded.

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
  ├── is_empty()               // len() == 0
  └── snapshot()               // read-only copy of all events
```

**Properties gained:**

| Property | EventLog |
|---|---|
| Multi-consumer | Yes — each subscriber independently tracks its offset |
| Late-join replay | Yes — subscribe from offset 0 gets full history |
| Persistence | Optional — JSONL logger is just another subscriber |
| Audit trail | Yes — `snapshot()` returns all events after run completes |
| Backpressure | N/A — Vec grows, consumers read at their own pace. ~200 bytes/event, thousands per run = single-digit MB |
| Consumer API | `Option<Event>` (simpler than channel `Result` types) |

**Notification mechanism:** `tokio::sync::watch<()>` notifies consumers that new events are available. When a consumer has caught up to the Vec length, it waits on `watch::changed()`. No polling. The watch value is `()` — the Vec itself is the source of truth for event count.

---

## Implementation Guide

### Step 0: Switch Epic to Single-Threaded Tokio Runtime

**Prerequisite.** Epic currently uses `#[tokio::main]` which defaults to a multi-threaded runtime. The EventLog design uses `std::sync::RwLock` for `Send + Sync` compliance, with the assumption that the lock is never contended in practice. A multi-threaded runtime breaks this assumption — `emit()` and `recv()` could race across OS threads.

**Change:** Replace `#[tokio::main]` with `#[tokio::main(flavor = "current_thread")]` in `epic/src/main.rs`. This matches the other backlot binaries (flick-cli, reel-cli, vault-cli all use `current_thread`).

**Why this works:** Epic's two `tokio::spawn` calls (orchestrator + TUI) do not require parallel OS threads. On a single-threaded runtime, spawned tasks interleave cooperatively at `.await` points, which is sufficient — the orchestrator yields at every LLM call, and the TUI yields in its `tokio::select!` loop. No CPU-bound parallel work occurs.

**Test impact:** Run the full epic test suite and TUI manually after the switch to verify no latent dependency on thread-parallelism.

### Step 1: Split Event Enum Between Cue and Epic

Replace cue's 24-variant `Event` with a 10-variant `CueEvent` (orchestration events only). Create epic's own `Event` enum (all 24 variants) with `From<CueEvent>` mapping. Make cue's `Orchestrator` generic over `T: EventEmitter<CueEvent>` instead of holding a concrete `EventSender`. No changes to `TaskStore`. See the "Prerequisite: Split Event Enum Between Cue and Epic" section for the full variant breakdown and adapter pattern.

### Step 2: Create `traits` Crate

Create the `traits` crate with `EventEmitter<E>`. Add it as a dependency of cue and epic. See the "Architecture" section above.

### Step 3: Add Serialize/Deserialize to Event

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

### Step 4: Implement EventLog and EventSubscription

```rust
// events.rs
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

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
    pub fn len(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.events.read().unwrap().is_empty()
    }

    /// Read-only snapshot of all events.
    pub fn snapshot(&self) -> Vec<Event> {
        self.events.read().unwrap().clone()
    }
}

impl EventSubscription {
    /// Receive the next event. Blocks until one is available.
    ///
    /// Note: `watch::Receiver::changed()` considers the initial channel value as
    /// unseen, so the first call returns immediately — causing one harmless spin
    /// iteration when a subscription starts already caught up. The loop re-checks
    /// the Vec and blocks correctly on the second `changed()` call.
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
            if self.notify_rx.changed().await.is_err() {
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
- **`std::sync::RwLock`** exists for `Send + Sync` compliance. After Step 0, epic runs on a single-threaded tokio runtime — spawned tasks interleave at `.await` points, never in parallel — so the lock is never contended. `tokio::spawn` requires `Send`, and `Arc<Vec<Event>>` is not `Sync`, so `RwLock` provides the necessary `Sync` impl. `std::sync` (not `tokio::sync`) because the lock is never held across an `.await` — `emit()` is synchronous, matching the current `let _ = sender.send(event)` pattern.
- **`emit()` takes ownership** of the event. No clone needed on the producer side.
- **`Event` needs `Clone`** only for the subscription side (`recv`/`try_recv` clone from the Vec). This is already derived on `Event`.
- **`try_recv` uses `read().ok()?`** — returns `None` if the lock is poisoned or contended, which is the correct non-blocking behavior.

### Step 5: Update send sites

All send sites currently use `let _ = sender.send(event)`. Change to `let _ = log.emit(event)`.

This is a mechanical rename. `emit()` is sync and infallible (panics only on poisoned lock, which indicates a bug). The `let _ =` pattern drops the returned offset, matching the current fire-and-forget semantics.

### Step 6: Update receive sites

**TUI** (`tui/mod.rs`): Currently uses `event_rx.recv()` inside a `tokio::select!` loop alongside crossterm input and a tick interval.

```rust
// Before (mpsc)
tokio::select! {
    _ = tick.tick() => {}
    ct_event = ct_stream.next() => { /* handle crossterm input */ }
    event = event_rx.recv() => {
        match event {
            Some(event) => self.handle_event(event),
            None => { self.orchestrator_done = true; }
        }
    }
}

// After (EventSubscription)
tokio::select! {
    _ = tick.tick() => {}
    ct_event = ct_stream.next() => { /* handle crossterm input */ }
    event = subscription.recv() => {
        match event {
            Some(event) => self.handle_event(event),
            None => { self.orchestrator_done = true; }
        }
    }
}
```

**Shutdown semantics:** `subscription.recv()` returns `None` when the internal `watch` sender is dropped (all `EventLog` clones dropped) AND all buffered events have been consumed. This matches the current mpsc behavior where `recv()` returns `None` after the sender is dropped and the channel is drained.

**Headless mode** (`main.rs`): Currently drops the receiver (`drop(rx)`). After migration, simply don't create a subscription — `EventLog` with no subscribers works fine (events accumulate in the Vec for post-run `snapshot()`).

### Step 7: Wire up EventLog

```rust
// main.rs
let log = EventLog::new();

let tui_sub = log.subscribe();  // TUI consumer — replays from 0

// Pass log.clone() to Orchestrator (producer)
// Pass tui_sub to TUI
```

The multi-consumer design supports future additions (JSONL file logger, web dashboard) as additional `log.subscribe()` calls — no changes to `EventLog` or existing consumers required.

### Step 8: Update tests

Tests that create `event_channel()` and assert on received events need updated to use `EventLog::new()` + `subscribe()`. Tests that discard the receiver need no changes — `EventLog` with no subscribers works fine (events accumulate in the Vec).

### Step 9: Post-run analysis

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

### Prerequisite: Split Event Enum Between Cue and Epic

The `Event` enum currently lives in `cue/src/events.rs` with 24 variants in a single enum. Cue is a generic orchestration framework — it should own only orchestration-level events, not application-specific ones.

**Event ownership after split:**

Cue defines `CueEvent` (10 variants — the events emitted by `orchestrator.rs`):
- `TaskRegistered`, `PhaseTransition`, `TaskCompleted`, `TaskLimitReached`
- `PathSelected`, `ModelSelected`
- `SubtasksCreated`, `BranchFixRound`, `FixSubtasksCreated`, `RecoverySubtasksCreated`

Epic defines `Event` (24 variants total — 14 epic-specific + mapped cue events):
- **Epic-specific** (emitted by `task/node_impl.rs` and `main.rs`): `VaultBootstrapCompleted`, `VaultRecorded`, `VaultReorganizeCompleted`, `UsageUpdated`, `FixModelEscalated`, `ModelEscalated`, `CheckpointAdjust`, `CheckpointEscalate`, `FixAttempt`, `DiscoveriesRecorded`, `RetryAttempt`, `FileLevelReviewCompleted`, `RecoveryStarted`, `RecoveryPlanSelected`
- **Mapped from CueEvent** (via `From<CueEvent>` impl): the 10 cue variants above, mapped 1:1 into epic's `Event` enum

**Migration:**
1. Replace `cue/src/events.rs` with a `CueEvent` enum containing only the 10 orchestration variants. Remove `EventSender`, `EventReceiver`, `event_channel()` — cue emits via the `EventEmitter<CueEvent>` trait.
2. In `epic/src/events.rs`, replace the re-export with epic's own `Event` enum (all 24 variants). Implement `From<CueEvent> for Event`.
3. Cue's orchestrator takes `T: EventEmitter<CueEvent>`. Epic provides an adapter that maps `CueEvent` → `Event` and appends to `EventLog`:

```rust
// In epic — adapter that bridges cue events into epic's EventLog.
// The trait's emit() delegates to EventLog's inherent emit(Event) method.
// In Rust, self.emit() resolves to the inherent method (not the trait method)
// because inherent methods take precedence — so this is unambiguous at the
// call site despite the shared name.
impl EventEmitter<CueEvent> for EventLog {
    fn emit(&self, event: CueEvent) {
        self.emit(Event::from(event));
    }
}
```

4. Epic's `task/node_impl.rs` emits `Event` variants directly into the `EventLog` (no adapter needed — it already knows epic's types).

### Architecture: `traits` Crate + `EventEmitter<E>`

A shared `traits` crate defines the minimal emit contract. All crates that need to emit events depend on `traits` — nothing else. `EventLog` stays in epic, which is the only crate that needs subscriptions, replay, and snapshot.

```rust
// traits crate — depended on by cue, epic, and any future emitters
pub trait EventEmitter<E>: Send + Sync {
    fn emit(&self, event: E);
}
```

**Why `Send + Sync`?** The orchestrator runs in a `tokio::spawn`ed task, which requires `Send`. The emitter is shared (via `Clone`/`Arc`), which requires `Sync`. Bounding the trait rather than each call site keeps the constraint visible and centralized.

**Why a trait?** The orchestrator (cue) only needs to *emit* events, never subscribe. Giving cue a concrete `EventLog` would pull subscription/replay infrastructure into a crate that doesn't use it. A trait keeps cue decoupled — it calls `emit()` without knowing what's behind it.

**Why in a separate crate?** Cue shouldn't own the trait (reel/flick would then depend on cue). Epic shouldn't own it (lower crates would depend on epic). A leaf `traits` crate has no dependencies and can be depended on by everything.

### Dependency Graph

```
traits (no dependencies)
  ▲
  ├── cue (Orchestrator takes impl EventEmitter<E>)
  ├── epic (EventLog implements EventEmitter<epic::Event>)
  ├── reel (future: takes impl EventEmitter<reel::AgentEvent>)
  └── flick (future: takes impl EventEmitter<flick::ModelEvent>)
```

### Wiring in Epic

Epic owns the `EventLog` and all subscription/consumer logic. It passes `EventLog` (or adapters) to lower crates at construction time.

```
epic (creates EventLog, distributes subscriptions to consumers)
  │
  ├── let log = EventLog::new();
  │
  ├── cue::Orchestrator receives log.clone() as impl EventEmitter<CueEvent>
  │     └── orchestrator calls transmitter.emit(CueEvent::...) — adapter maps to epic::Event
  │
  ├── epic's TaskRuntime holds log.clone() directly, emits epic::Event via inherent method
  │
  ├── tui_sub = log.subscribe();        ◄──── TUI replays from 0
  ├── logger_sub = log.subscribe();     ◄──── JSONL logger replays from 0
  └── web_sub = log.subscribe_from(n);  ◄──── Late-joining dashboard replays from offset n
```

### Cue's Orchestrator

Cue takes a generic transmitter, not a concrete EventLog. The orchestrator is generic over `EventEmitter<CueEvent>` — it emits cue's own event type and never sees epic's types:

```rust
// In cue crate
pub struct Orchestrator<S: TaskStore, T: EventEmitter<CueEvent>> {
    store: S,
    transmitter: T,
    limits: LimitsConfig,
    state_path: Option<PathBuf>,
}
```

No changes to `TaskStore` — it keeps only `type Task: TaskNode`. Cue's current `emit()` helper method (`let _ = self.events.send(event)`) becomes `self.transmitter.emit(event)`. Cue never sees `EventLog`, `EventSubscription`, or epic's `Event` enum. The mapping from `CueEvent` to `epic::Event` lives entirely in epic's adapter (see "Prerequisite: Split Event Enum" above).

### Future Crate Event Propagation

Reel and vault emit no events today. If they grow to need mid-execution notifications, each crate defines its own event enum and takes an `impl EventEmitter<LocalEvent>`. Epic bridges via an adapter:

```rust
// In epic — adapter that maps reel events into epic's EventLog
struct ReelEmitter(EventLog);

impl EventEmitter<reel::AgentEvent> for ReelEmitter {
    fn emit(&self, event: reel::AgentEvent) {
        self.0.emit(epic::Event::from(event));  // From impl does the mapping
    }
}
```

This keeps each crate independent:
- Reel depends on `traits`, not epic or cue
- Epic owns the mapping logic (it's the only crate that knows all event types)
- EventLog's inherent `emit(Event)` is the single append path — all trait impls (`EventEmitter<CueEvent>`, future `EventEmitter<reel::AgentEvent>`, etc.) convert and delegate to it
- Adapters handle cross-type bridging at the boundary

---

## Migration Summary

| Step | What changes | Lines changed (est.) | Risk |
|---|---|---|---|
| 0 | Switch epic to `current_thread` tokio runtime | ~5 | Low |
| 1 | Split `Event` enum: `CueEvent` in cue, `Event` in epic, `From` mapping | ~60 | Medium |
| 2 | Create `traits` crate with `EventEmitter<E>` trait | ~15 new | None |
| 3 | Add `Serialize`/`Deserialize` derives to Event | ~10 | None |
| 4 | Add EventLog + EventSubscription in epic | ~80 new | Low |
| 5 | Change send sites (`send` → `emit`) | ~40 (rename) | Low |
| 6 | Change consumer receive calls | ~20 | Low |
| 7 | Wire up EventLog in main.rs, pass adapter to orchestrator, subscription to TUI | ~15 | Low |
| 8 | Update test event assertions | ~30 | Low |
| 9 | Post-run snapshot usage | ~10 new | Low |
| **Total** | | **~285** | **Low** |

### Ordering Relative to Orchestrator Extraction

EventLog stays in epic. The `EventEmitter<E>` trait goes in the `traits` crate. Cue's orchestrator takes a generic `impl EventEmitter<E>` — it never depends on EventLog directly.

This can be implemented independently of the orchestrator extraction. The extraction's preparatory phases (decision collapsing, cross-task queries, type decoupling, trait definitions, boundary verification) do not touch the event channel type. No conflict.

---

## Research Sources

- Tokio channel documentation (mpsc, broadcast, watch semantics)
- Rust community patterns for event-driven systems (users.rust-lang.org)
- Production orchestrator architectures (Temporal, Prefect, Airflow event sourcing patterns)
- Implementation pattern comparison: Vec+watch vs Vec+Notify vs broadcast+Vec hybrid
