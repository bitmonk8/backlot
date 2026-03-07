# Project Audit

## Purpose

Full audit of the Epic codebase at v1 completion. Goals: identify correctness issues, stale/dead code, doc drift, security gaps, design fidelity, and simplification opportunities before real-world use.

## Approach

The audit is organized as a **review matrix**: review types on one axis, code units on the other. Each cell is a focused task for a single agent. Not every cell applies — cells marked `--` are intentionally skipped.

---

## Code Units

| ID | Unit | Files | Description |
|----|------|-------|-------------|
| U1 | orchestrator | `orchestrator.rs` | DFS loop, retry/escalation, fix loops, recovery, checkpoints, resume |
| U2 | agent/flick | `agent/flick.rs` | FlickClient wrapper, tool loop, structured output, resume |
| U3 | agent/config_gen | `agent/config_gen.rs` | Wire types, TryFrom conversions, JSON schemas |
| U4 | agent/prompts | `agent/prompts.rs` | Prompt assembly for all agent methods |
| U5 | agent/tools | `agent/tools.rs` | 6 tool implementations, path sandboxing, size limits |
| U6 | agent/mod | `agent/mod.rs` | AgentService trait |
| U7 | task | `task/*.rs` | Task struct, phases, Magnitude, LeafResult, subtypes |
| U8 | state | `state.rs` | EpicState persistence, atomic writes, load/save |
| U9 | events | `events.rs` | Event enum, all variants |
| U10 | config | `config/*.rs` | EpicConfig, VerificationStep, TOML loading |
| U11 | init | `init.rs` | Agent-driven interactive scaffolding |
| U12 | cli + main | `cli.rs`, `main.rs` | Clap CLI, wiring, TUI/headless split, shutdown |
| U13 | tui | `tui/*.rs` | TuiApp, task tree, worklog, metrics panels |
| U17 | docs | `docs/*.md` | Design documents |

## Review Types

| ID | Type | Focus |
|----|------|-------|
| R1 | **Correctness** | Logic errors, edge cases, unsound assumptions, off-by-one, missed error paths |
| R2 | **Security** | Injection, sandboxing, TOCTOU, credential exposure, resource exhaustion |
| R3 | **Error handling** | Panics (unwrap/expect), error propagation, graceful degradation, resource cleanup |
| R4 | **Dead code & cruft** | Unused code, stubs, stale comments, stale allow annotations |
| R5 | **Simplification** | Unnecessary complexity, duplicated logic, boilerplate reduction, extraction opportunities |
| R6 | **Testability** | Mock boundaries, test friction, isolation, missing coverage, state machine clarity |
| R7 | **Design intent** | Fidelity to EPIC_DESIGN2, agent autonomy balance, cost control, verification enforcement |
| R8 | **Doc consistency** | Design doc matches implementation, stale references, missing updates |

---

## Review Matrix

Each cell is one focused agent task. All 95 cells complete. Detailed findings in `docs/audit/*.md`.

| Unit | R1 | R2 | R3 | R4 | R5 | R6 | R7 | R8 |
|------|----|----|----|----|----|----|----|----|
| **U1** orchestrator | [x] | [x] | [x] | [x] | [x] | [x] | [x] | -- |
| **U2** agent/flick | [x] | [x] | [x] | [x] | [x] | [x] | [x] | -- |
| **U3** agent/config_gen | [x] | -- | [x] | [x] | [x] | [x] | -- | -- |
| **U4** agent/prompts | [x] | -- | [x] | [x] | [x] | [x] | [x] | -- |
| **U5** agent/tools | [x] | [x] | [x] | [x] | [x] | [x] | -- | -- |
| **U6** agent/mod | [x] | -- | [x] | [x] | -- | [x] | [x] | -- |
| **U7** task | [x] | -- | [x] | [x] | [x] | [x] | [x] | -- |
| **U8** state | [x] | [x] | [x] | [x] | -- | [x] | -- | -- |
| **U9** events | [x] | -- | -- | [x] | [x] | -- | -- | -- |
| **U10** config | [x] | -- | [x] | [x] | -- | [x] | -- | -- |
| **U11** init | [x] | -- | [x] | [x] | [x] | [x] | -- | -- |
| **U12** cli + main | [x] | [x] | [x] | [x] | [x] | [x] | -- | -- |
| **U13** tui | [x] | -- | [x] | [x] | [x] | [x] | -- | -- |
| **U17** docs | -- | -- | -- | [x] | -- | -- | [x] | [x] |

Cross-cutting: X1 (Cargo.toml), X2 (clippy pedantic), X3 (compiler warnings), X4 (CI readiness), X5 (global patterns), X6 (constants vs config). Broad-lens: B1–B4 (simplification), B5–B8 (design).

---

## Audit Results Summary

541 original findings. 99 resolved fully, 10 partially (3 remaining partial).

### Current Findings by Severity

| Severity | Still Valid | Partially Resolved |
|----------|------------|-------------------|
| Critical | 0 | 0 |
| Major | 41 | 3 |
| Minor | 223 | 7 |
| Note | 164 | 3 |
| **Total** | **428** | **13** |

### Remaining Findings by Category

| Category | Approx count | Critical | Major |
|---|---|---|---|
| Operational correctness sandboxing (Frida, TOCTOU, per-phase enforcement) | ~33 | 0 | ~16 |
| Testability (no injection seams, zero coverage in init/TUI/main/state) | ~65 | 0 | ~15 |
| Simplification/dedup (retry loops, event variants, prompt boilerplate) | ~61 | 0 | ~3 |
| Error handling (inconsistent fatal vs best-effort, panics, silent swallowing) | ~40 | 0 | ~3 |
| Dead code/stubs (unused ToolGrant flags) | ~20 | 0 | ~2 |
| Design intent gaps (prompt content, tool grants, missing review phase) | ~27 | 0 | 0 |
| Doc drift (TUI event names, CLI syntax, type mismatches) | ~19 | 0 | 0 |
| Correctness (cycle detection, phase transitions) | ~22 | 0 | 0 |
| Other (clippy, naming, notes) | ~128 | 0 | 0 |

---

## Major Findings (41 still valid, 3 partially resolved)

### Operational Correctness & Sandboxing

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` — race between validation and open | `tools.rs` |
| U2-R2#2 | TOCTOU in `write_file` path validation — path validated then file written non-atomically | `tools.rs` |
| U2-R2#3 | TOCTOU in `edit_file` between read and write — file may change between operations | `tools.rs` |

### Testability

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R6#1 | No filesystem abstraction for tool testing — all tests require real FS | `tools.rs` |
| U5-R6#2 | No process execution abstraction for bash testing — tests spawn real shells | `tools.rs` |
| U2-R6#1 | `build_client` hard-codes `flick::resolve_provider` with no injection point | `flick.rs` |
| U2-R6#2 | `run_with_tools` tool-loop logic not unit-testable — tightly coupled to FlickClient | `flick.rs` |
| U2-R6#3 | `tools::execute_tool` called directly with no indirection for test isolation | `flick.rs` |
| U1-R6#1 | `check_scope_circuit_breaker` shells out to `git` directly — untestable without real repo | `orchestrator.rs` |
| U6-R6#1 | `MockAgentService` is private to `orchestrator::tests` — cannot reuse in other test modules | `orchestrator.rs` |
| U7-R6#1 | `TaskPhase` transitions unchecked — no `try_transition` guard, any transition silently succeeds | `task/mod.rs` |
| U7-R6#2 | `LeafResult` and `RecoveryPlan` lack `PartialEq` — cannot assert equality in tests | `task/mod.rs` |
| U7-R6#3 | Zero unit tests in task module | `task/` |
| U8-R6#1 | `save`/`load` coupled to real filesystem — no abstraction for test isolation | `state.rs` |
| U8-R6#2 | No error/failure path tests for save/load | `state.rs` |
| U10-R6#1 | No `PartialEq` derive on config structs — cannot assert equality in tests | `config/` |
| U10-R6#4 | `init.rs` prompt functions read from `io::stdin()` directly — untestable | `init.rs` |
| U11-R6 | Zero test coverage for entire init module (multiple findings) | `init.rs` |
| U13-R6 | Zero test coverage for entire TUI module (multiple findings) | `tui/` |
| U14-R6 | git module empty; scope check hardwired in orchestrator with no trait boundary (multiple findings) | `orchestrator.rs` |

### Simplification & Deduplication

All 6 majors resolved.

| Ref | Finding | Status |
|-----|---------|--------|
| U2-R5#2 | `execute_leaf`/`fix_leaf` dedup | **Resolved** — extracted `run_leaf_task` helper |
| U2-R5#3 | `design_and_decompose`/`design_fix_subtasks` dedup | **Resolved** — extracted `decompose_with_prompt` helper |
| U3-R5#1 | Subtask schema duplication | **Resolved** — extracted `subtask_schema()` helper |
| U3-R6#1 | `build_config` monolith | **Resolved** — split into `build_config_json` + thin wrapper |
| B3#1 | Redundant `VerificationStarted`/`VerificationComplete` events | **Resolved** — removed; `TaskCompleted`/`PhaseTransition` suffice |
| B3#2 | Redundant `SubtasksCreated` alongside recovery events | **Resolved** — removed redundant emission at recovery site |

### Dead Code & Stubs

| Ref | Finding | Status |
|-----|---------|--------|
| U2-R7#5 | `result.usage` never read | **Resolved** — added `log_usage` helper to surface token/cost data |

### Partially Resolved Majors

| Ref | Finding | Status |
|-----|---------|--------|
| U10-R1#2 | `LimitsConfig` values used but no comprehensive validation | Some fields clamped to min 1, but no full validate() method |
| U10-R6#2 | Config validation incomplete | Clamping added for some fields; no comprehensive boundary checks |
| U10-R6#3 | No dedicated config `load()` with filesystem abstraction | Config loaded in main.rs directly; no config-module-level load function |

---

## Recommended Action Items (Priority Order)

### ~~1. Simplification (6 majors + 1 dead code)~~ — RESOLVED

All 7 issues resolved. See Simplification & Dead Code sections above.

### 1. Config validation (3 partially resolved)

| Ref | Summary | Fix |
|-----|---------|-----|
| U10-R1#2 | `LimitsConfig` has no comprehensive validation | Add `validate()` with bounds checking |
| U10-R6#2 | Config validation incomplete — no boundary tests | Add `PartialEq` derives and boundary tests |
| U10-R6#3 | No dedicated config `load()` abstraction | Add `EpicConfig::load(path)` in config module |

### 2. Testability (16 majors)

Injection seams, test isolation, and missing coverage. Largest group — can be addressed incrementally.

| Ref | Summary | Fix |
|-----|---------|-----|
| U5-R6#1 | No filesystem abstraction for tool testing | Add FS trait or test helper |
| U5-R6#2 | No process execution abstraction for bash testing | Add process trait or test helper |
| U2-R6#1 | `build_client` hard-codes `flick::resolve_provider` | Add injection point |
| U2-R6#2 | `run_with_tools` tool-loop not unit-testable | Decouple from FlickClient |
| U2-R6#3 | `tools::execute_tool` called directly with no indirection | Add trait boundary |
| U1-R6#1 | `check_scope_circuit_breaker` shells out to `git` directly | Add git trait or injection |
| U6-R6#1 | `MockAgentService` private to `orchestrator::tests` | Move to shared test module |
| U7-R6#1 | `TaskPhase` transitions unchecked | Add `try_transition` guard |
| U7-R6#2 | `LeafResult` and `RecoveryPlan` lack `PartialEq` | Add `PartialEq` derive |
| U7-R6#3 | Zero unit tests in task module | Add tests |
| U8-R6#1 | `save`/`load` coupled to real filesystem | Add abstraction or test helpers |
| U8-R6#2 | No error/failure path tests for save/load | Add failure path tests |
| U10-R6#1 | No `PartialEq` derive on config structs | Add derive |
| U10-R6#4 | `init.rs` prompt functions read from `io::stdin()` directly | Accept `Read` trait param |
| U11-R6 | Zero test coverage for init module | Add tests |
| U13-R6 | Zero test coverage for TUI module | Add tests |
| U14-R6 | git module empty; scope check hardwired with no trait boundary | Add git trait |

### 3. Operational correctness sandboxing (Frida)

TOCTOU findings below have partial code mitigations possible (e.g., `O_NOFOLLOW`, `flock`), but full resolution requires Frida's per-phase syscall enforcement. Deferred until items 1–2 are addressed.

| Ref | Summary |
|-----|---------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` |
| U2-R2#2 | TOCTOU in `write_file` path validation |
| U2-R2#3 | TOCTOU in `edit_file` between read and write |
| *(full)* | Per-phase access policy enforcement via runtime interception. See [SANDBOXING.md](SANDBOXING.md) Concern 2. |
