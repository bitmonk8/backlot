# Project Status

## Current Phase

**Implementation** — Core orchestrator and agent wiring complete. Tool execution and state persistence next.

## Milestones

- [x] Design documents and open questions resolved (23/23)
- [x] Scaffold Rust project — module structure, dependencies, Clippy lints configured
- [x] Core orchestrator loop — task types, events, state, AgentService trait, DFS loop with retry/escalation, 6 tests passing
- [x] Agent runtime selected — ZeroClaw evaluated, audited, forked, then replaced by [Flick](https://github.com/bitmonk8/flick) (external executable, no crate dependency). Dependency count reduced from ~771 to ~104 packages.
- [x] Agent call wiring — `FlickAgent` implements `AgentService` via Flick subprocess invocation. Config generation (YAML), structured output schemas, wire format types with `TryFrom` conversions, prompt assembly, tool loop with resume. 38 tests passing.
- [ ] Tool execution — `execute_tool()` currently stubbed; wire real implementations for `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `bash`
- [ ] State persistence integration — Wire `EpicState::save`/`load` into the main run loop for session resume

## Next Work Candidates

1. **Tool execution** — Implement real tool backends behind `execute_tool()`. Currently all tools return a stub error. This is the remaining blocker for end-to-end runs.
2. **State persistence integration** — Wire `EpicState::save`/`load` into the main run loop for session resume.
3. **TUI event consumer** — Connect `EventReceiver` to ratatui task tree and worklog panels.
4. **Discoveries propagation** — `TaskOutcomeWire.discoveries` is parsed but dropped during `TryFrom` conversion. Requires `AgentService` trait signature change to carry discoveries alongside `TaskOutcome`.

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

**Deferred:** Tool execution (stubbed), discoveries propagation (requires trait change).
