# Project Status

## Current Phase

**Implementation** — Core orchestrator, agent wiring, tool execution, state persistence, TUI, discoveries propagation, CLI, and `epic init` complete.

## Milestones

- [x] Design documents and open questions resolved (23/23)
- [x] Scaffold Rust project — module structure, dependencies, Clippy lints configured
- [x] Core orchestrator loop — task types, events, state, AgentService trait, DFS loop with retry/escalation, 6 tests passing
- [x] Agent runtime selected — ZeroClaw evaluated, audited, forked, then replaced by [Flick](https://github.com/bitmonk8/flick) (external executable, no crate dependency). Dependency count reduced from ~771 to ~104 packages.
- [x] Agent call wiring — `FlickAgent` implements `AgentService` via Flick subprocess invocation. Config generation (YAML), structured output schemas, wire format types with `TryFrom` conversions, prompt assembly, tool loop with resume. 38 tests passing.
- [x] Tool execution — All 6 tools implemented: `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `bash`. Path sandboxing, size limits, timeout handling. 15 new tests (53 total).
- [x] State persistence integration — `EpicState` saves/loads via `.epic/state.json`. Orchestrator checkpoints after assessment, decomposition, child completion, and verification. `main.rs` resumes from persisted state or creates fresh. Atomic writes (write-rename), resume skips completed/failed/mid-execution tasks correctly, reuses existing decomposition, goal mismatch detection, corrupt state error handling. 5 new tests (58 total).
- [x] TUI event consumer — `TuiApp` consumes orchestrator events via ratatui + crossterm. Task tree panel (DFS with status indicators ✓/✗/▸/·, current-task cursor), worklog panel (timestamped event stream with follow-tail), metrics panel (toggle), header bar (goal, progress, elapsed). Keyboard controls: q/Ctrl-C quit, t toggle tail, m toggle metrics, ↑↓ scroll. Orchestrator runs in background tokio task, TUI in sync foreground loop. `EPIC_NO_TUI=1` for headless mode. `TaskRegistered` event added for TUI to build task tree from events alone.
- [x] Discoveries propagation — `AgentService::execute_leaf` returns `LeafResult` (outcome + discoveries). Orchestrator stores discoveries on tasks, emits `DiscoveriesRecorded` event, triggers checkpoint flow. Sibling context includes discoveries in prompt. Failed sibling reason extracted from attempts. 2 new tests (60 total).
- [x] CLI (clap) — Replaced ad-hoc env-var/arg parsing with proper `clap` derive CLI. Subcommands: `epic run <goal>`, `epic resume`, `epic status`. Global options: `--flick-path`, `--credential`, `--no-tui` (all with env-var fallbacks). `status` subcommand prints goal, root phase, and task counts from persisted state. 60 tests passing.
- [x] `epic init` — Agent-driven interactive configuration scaffolding. Flick agent (Sonnet, read-only tools) scans project for build/test/lint markers. Interactive CLI confirms/edits/skips each step, prompts for model preferences and depth/budget limits. Writes `epic.toml` with atomic write. Declined steps included as TOML comments. `EpicConfig` struct with `VerificationStep`, `ModelConfig`, `LimitsConfig`, `AgentConfig`, `ProjectConfig`. 3 new tests (63 total).

## Next Work Candidates

No remaining work candidates. All planned milestones complete.

## Decisions Made

### 2026-02-25: Configuration format — TOML

**Decision:** TOML for all Epic configuration files (`epic.toml`, `.epic/config.toml`).

**Rationale:** Rust ecosystem standard. Shallow config fits naturally. `toml` crate is mature and serde-native. YAML rejected (archived crate, implicit type coercion). RON rejected (too niche).

### 2026-02-25: `epic init` — agent-driven interactive scaffolding

**Decision:** `epic init` uses an agent to explore the project (build system markers, test frameworks, linters, CI config), presents findings, and interactively confirms verification steps. Writes `epic.toml`.

### 2026-02-25: Batch decisions — Rust, Scope, Document Store, TUI

**Rust-specific:** tokio (ecosystem standard), anyhow + thiserror, serde + serde_json + toml.

**Scope (v1 boundaries):** No parallel execution, no multi-language special handling, no git hosting integration.

**Document Store:** File-based (markdown) for v1. Librarian via Flick agent (Haiku, read-only tools).

**TUI:** ratatui + crossterm. Read-only monitoring for v1.

**Config ownership:** Epic owns all config. `[agent]` section in epic.toml for runtime knobs.

### 2026-02-26: Core orchestrator loop implemented

**Scope:** Task types, assessment/branch/verify data structs, `Event` enum with unbounded channel, `EpicState` with JSON persistence, `AgentService` trait (6 async methods), `Orchestrator<A: AgentService>` with DFS loop, retry/escalation (3 retries per tier, Haiku→Sonnet→Opus).

**Tests (6):** single_leaf, two_children, leaf_retry_and_escalation, terminal_failure, depth_cap_forces_leaf, persistence_round_trip.

**Deferred for v1:** Fix loop after verification failure, full recovery re-decomposition, checkpoint adjust/escalate actions.

### 2026-03-04: Replace ZeroClaw with Flick

**Decision:** Replace ZeroClaw (library dependency via forked submodule) with [Flick](https://github.com/bitmonk8/flick) (external executable, subprocess invocation).

**Rationale:** ZeroClaw had provenance concerns (star farming, 12-day-old project, crypto fraud ecosystem), heavy transitive dependency tree (~771 packages), fork maintenance burden, and intermittent compiler ICE. Flick as an external executable eliminates the crate dependency entirely.

### 2026-03-04: Agent call wiring implemented

**Scope:** `FlickAgent` implementing `AgentService` trait. 8 files modified/created:
- `models.rs` — Model → Flick model ID mapping (Haiku/Sonnet/Opus) and token limits
- `tools.rs` — `ToolGrant` bitflags, tool definitions for Flick config, stubbed `execute_tool()`
- `config_gen.rs` — `FlickConfig` YAML generation, 6 wire format types (`AssessmentWire`, `DecompositionWire`, etc.) with `TryFrom` conversions, 6 JSON output schemas, `write_config()`
- `prompts.rs` — Prompt assembly (system prompt + query) for all 6 `AgentService` methods
- `flick.rs` — `FlickAgent` struct with `invoke_flick()`, `run_structured()`, `run_with_tools()` (tool loop with 50-round cap, `kill_on_drop`, timeout handling), full `AgentService` impl
- `main.rs` — Wires `FlickAgent` into `Orchestrator` with env-var config

**Tests:** 32 new unit tests (38 total). 0 clippy warnings.

**Deferred:** Tool execution (stubbed). Discoveries propagation deferred (required trait change) — implemented in later milestone.

### 2026-03-04: Tool execution implemented

**Scope:** All 6 tools implemented in `tools.rs`: `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `bash`. `flick.rs` call site updated from sync to async.

**Security:** Path sandboxing via `safe_path()` (canonicalization + containment check), `verify_ancestors_within_root()` for write_file, symlink guard on `allow_new_file` paths, `follow_links(false)` on directory walks.

**Resource limits:** `read_file` streams first 256 KiB only (no full-file load), `grep` skips files >10 MiB, `glob` caps at 1000 results, `grep` caps output at 64 KiB, `bash` output capped at 64 KiB with char-boundary-safe truncation, bash timeout clamped to 600s max.

**Async:** `glob`/`grep` use `spawn_blocking` to avoid blocking the tokio runtime with sync `WalkDir`/`std::fs` I/O.

**Tests:** 15 new tests (53 total). 0 clippy warnings.

### 2026-03-05: State persistence integration

**Scope:** `EpicState` gains `root_id` field for session resume. `Orchestrator` gains `state_path: Option<PathBuf>` and `checkpoint_save()` method. `main.rs` loads from `.epic/state.json` if present, otherwise creates fresh state. Extracted `finalize_task` method for verification/completion logic.

**Checkpoint locations:** After assessment (path/model applied), after decomposition (subtasks created), after each task completion/failure (verification pass/fail, execution failure).

**Resume semantics:** `execute_task` returns early for `Completed`/`Failed` tasks. `Verifying` tasks skip to re-verification (not re-execution). `Executing` tasks with `path` already set skip re-assessment. `execute_branch` reuses existing `subtask_ids` (skips `design_and_decompose`), skips terminal children. `Orchestrator::run()` sets `root_id` on state.

**Atomic writes:** `save()` writes to `.json.tmp` then renames. Prevents corruption on kill.

**Goal mismatch:** CLI goal compared against persisted root goal; exits with diagnostic on mismatch.

**Corrupt state:** `EpicState::load()` errors produce human-readable message and exit, not raw serde trace.

**Best-effort checkpoints:** `checkpoint_save()` logs errors to stderr but does not abort the run.

**Deferred for v1:** Leaf retry counter resets on resume (bounded overshoot). No checkpoint during leaf retry loop (intermediate attempts lost on crash).

**Tests:** 5 new tests: `checkpoint_saves_state`, `resume_skips_completed_child`, `resume_skips_decomposition_when_subtasks_exist`, `resume_mid_execution_branch_not_reassessed`, `resume_verifying_skips_execution` (58 total). 0 clippy warnings.

### 2026-03-05: TUI event consumer implemented

**Scope:** `TuiApp` in `tui/mod.rs` consumes `EventReceiver` and renders via ratatui + crossterm. 4 files implemented/rewritten, 2 files modified.

**Layout:** Header (status, goal, progress counter, elapsed time), body (task tree + worklog, optionally + metrics), footer (keybindings).

**Task tree:** DFS-ordered display built from `TaskRegistered` events. Status indicators: ✓ completed, ✗ failed, ▸ in progress, · pending. Current-task cursor (←). Scrollable via ↑↓.

**Worklog:** Timestamped event stream with color-coded entries (green=success, red=error, yellow=warn). Follow-tail toggle (t key).

**Metrics panel:** Toggle (m key). Shows task counts by phase (total, completed, in-progress, pending, failed).

**Event additions:** `TaskRegistered { task_id, parent_id, goal, depth }` added to `Event` enum. Emitted by orchestrator for root task, pre-existing subtasks (resume), and newly created subtasks.

**Architecture:** Orchestrator runs in background `tokio::spawn` task. TUI runs in `spawn_blocking` (not on async runtime). Events drained non-blockingly via `try_recv`. `EPIC_NO_TUI=1` env var for headless mode (original behavior). On TUI quit, orchestrator gets 2s grace period then `abort_handle.abort()`.

**Safety:** Panic hook restores terminal (raw mode + alternate screen) before default handler. UTF-8 safe truncation via `char_indices().take_while()` on full string. Scroll clamped in both key handler and render. Worklog capped at 10,000 entries. `TaskCompleted` defensively sets phase regardless of event ordering. `TaskPhase` derives `Copy`.

**Files modified:** `events.rs` (new variant), `orchestrator.rs` (emit `TaskRegistered`, `Copy` cleanup), `main.rs` (TUI/headless mode split, `spawn_blocking`, abort), `task/mod.rs` (`TaskPhase` + `Copy`), `tui/mod.rs`, `tui/task_tree.rs`, `tui/worklog.rs`, `tui/metrics.rs`.

**Tests:** 58 passing (no new tests — TUI is UI code, tested via existing orchestrator event emission tests). 0 clippy warnings.

### 2026-03-05: Discoveries propagation implemented

**Scope:** Leaf execution results now carry discoveries alongside the task outcome. `AgentService::execute_leaf` returns `LeafResult` (outcome + discoveries) instead of bare `TaskOutcome`. Orchestrator stores discoveries on the task and triggers checkpoint flow for sibling coordination.

**Changes:**
- `task/mod.rs` — Added `LeafResult` struct (outcome + discoveries)
- `agent/mod.rs` — `execute_leaf` return type changed to `LeafResult`
- `agent/config_gen.rs` — `TryFrom<TaskOutcomeWire>` now produces `LeafResult`, extracting discoveries from wire format
- `agent/flick.rs` — `FlickAgent::execute_leaf` returns `LeafResult`
- `orchestrator.rs` — `execute_leaf` destructures `LeafResult`, stores discoveries on task, emits `DiscoveriesRecorded` event
- `events.rs` — Added `DiscoveriesRecorded { task_id, count }` variant
- `tui/mod.rs` — Handles `DiscoveriesRecorded` in worklog

**Data flow:** Agent returns `TaskOutcomeWire` with optional `discoveries` → `TryFrom` extracts into `LeafResult` → orchestrator stores on `Task.discoveries` → `execute_branch` reads child discoveries → calls `checkpoint()` with them → sibling context includes discoveries via `SiblingSummary` → prompt formatting shows discoveries to subsequent tasks.

**Tests:** 2 new tests: `task_outcome_wire_with_discoveries` (wire conversion), `discoveries_propagated_to_checkpoint` (end-to-end: leaf reports discoveries → stored on task → checkpoint called → event emitted). 60 total. 0 clippy warnings.

### 2026-03-05: CLI (clap) implemented

**Scope:** Replaced ad-hoc `std::env::args()` + env-var parsing with `clap` derive-based CLI.

**Subcommands:**
- `epic run <goal>` — Start a new run. If state file exists with same goal, resumes transparently. Different goal prints diagnostic and exits.
- `epic resume` — Resume from `.epic/state.json`. Exits with message if no state file found.
- `epic status` — Prints goal, root phase, and task counts (completed/in-progress/pending/failed) from persisted state. No agent or Flick needed.

**Global options:** `--flick-path` (env: `EPIC_FLICK_PATH`, default: `flick`), `--credential` (env: `EPIC_CREDENTIAL`, default: `anthropic`), `--no-tui` (env: `EPIC_NO_TUI`).

**Files:** New `cli.rs` module. `main.rs` rewritten. `Cargo.toml` added `env` feature to clap.

**Tests:** 60 passing (no new tests — CLI is integration surface). 0 clippy warnings.

### 2026-03-05: `epic init` implemented

**Scope:** Agent-driven interactive configuration scaffolding. New `Init` subcommand. 3 files created, 5 files modified.

**Flow:**
1. Early `epic.toml` existence check (before Flick agent construction)
2. `FlickAgent::explore_for_init()` — Sonnet agent with read-only tools scans project for build system markers, test frameworks, linters, CI config
3. `InitFindingsWire` structured output with `DetectedStepWire` entries
4. Interactive confirmation: accept/edit/skip each step, add custom steps
5. `prompt_models()` — model preference confirmation (accept defaults or customize)
6. `prompt_limits()` — depth/budget limit confirmation (accept defaults or customize)
7. Atomic write (`epic.toml.tmp` → rename → `epic.toml`)
8. Declined steps appended as TOML comments for reference

**Config types:** `EpicConfig` (top-level), `VerificationStep`, `ModelConfig`, `LimitsConfig`, `AgentConfig`, `ProjectConfig` — all serde Serialize/Deserialize with TOML defaults.

**Files created:** `init.rs`, `config/project.rs` (rewritten from stub).
**Files modified:** `cli.rs` (Init subcommand), `main.rs` (wiring), `agent/config_gen.rs` (wire types + schema), `agent/flick.rs` (explore method), `agent/mod.rs` + `config/mod.rs` (visibility).

**Error handling:** `read_line_checked` propagates I/O errors for critical prompts. `read_line_or_eof` propagates I/O errors, treats EOF as empty. Init uses `bail!()` not `process::exit()`.

**Tests:** 3 new tests (63 total): `default_config_round_trips`, `parse_with_verification_steps`, `parse_minimal_config`. 0 clippy warnings in new code.
