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

541 original findings. 119 resolved fully, 10 partially resolved (7 minor, 3 note — 0 major partial remain).

### Current Findings by Severity

| Severity | Still Valid | Partially Resolved |
|----------|------------|-------------------|
| Critical | 0 | 0 |
| Major | 21 | 0 |
| Minor | 223 | 7 |
| Note | 164 | 3 |
| **Total** | **408** | **10** |

### Remaining Findings by Category

| Category | Approx count | Critical | Major |
|---|---|---|---|
| Operational correctness sandboxing (lot, TOCTOU, per-phase enforcement) | ~33 | 0 | ~16 |
| Simplification/dedup (retry loops, event variants, prompt boilerplate) | ~61 | 0 | ~3 |
| Error handling (inconsistent fatal vs best-effort, panics, silent swallowing) | ~40 | 0 | ~3 |
| Dead code/stubs (unused ToolGrant flags) | ~20 | 0 | ~2 |
| Design intent gaps (prompt content, tool grants, missing review phase) | ~27 | 0 | 0 |
| Doc drift (TUI event names, CLI syntax, type mismatches) | ~19 | 0 | 0 |
| Correctness (cycle detection, phase transitions) | ~22 | 0 | 0 |
| Other (clippy, naming, notes) | ~128 | 0 | 0 |

---

## Major Findings (21 still valid, 0 partially resolved)

### Operational Correctness & Sandboxing

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` — race between validation and open | `tools.rs` |
| U2-R2#2 | TOCTOU in `write_file` path validation — path validated then file written non-atomically | `tools.rs` |
| U2-R2#3 | TOCTOU in `edit_file` between read and write — file may change between operations | `tools.rs` |

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

### ~~Partially Resolved Majors~~ — ALL RESOLVED

| Ref | Finding | Status |
|-----|---------|--------|
| U10-R1#2 | `LimitsConfig` values used but no comprehensive validation | **Resolved** — `EpicConfig::validate()` with bounds checking, called from `load()` |
| U10-R6#2 | Config validation incomplete | **Resolved** — `PartialEq`+`Eq` derives on all config structs, 14 boundary tests |
| U10-R6#3 | No dedicated config `load()` with filesystem abstraction | **Resolved** — `EpicConfig::load(path)` replaces inline loading in main.rs |

---

## Recommended Action Items (Priority Order)

### ~~Simplification (6 majors + 1 dead code)~~ — RESOLVED

All 7 issues resolved. See Simplification & Dead Code sections above.

### ~~Config validation (3 partially resolved)~~ — RESOLVED

| Ref | Summary | Status |
|-----|---------|--------|
| U10-R1#2 | `LimitsConfig` has no comprehensive validation | **Resolved** — `EpicConfig::validate()` with bounds checking |
| U10-R6#2 | Config validation incomplete — no boundary tests | **Resolved** — `PartialEq`+`Eq` derives, 14 boundary tests |
| U10-R6#3 | No dedicated config `load()` abstraction | **Resolved** — `EpicConfig::load(path)` in config module |

### 1. Operational correctness sandboxing (lot)

TOCTOU findings below have partial code mitigations possible (e.g., `O_NOFOLLOW`, `flock`), but mitigated by lot's per-phase process sandboxing.

| Ref | Summary |
|-----|---------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` |
| U2-R2#2 | TOCTOU in `write_file` path validation |
| U2-R2#3 | TOCTOU in `edit_file` between read and write |
| *(full)* | Per-phase access policy enforcement via lot process sandboxing. See [SANDBOXING.md](SANDBOXING.md) Concern 2. |
