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

Each cell is one focused agent task.

---

## Major Findings

### Operational Correctness & Sandboxing

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` — race between validation and open | `tools.rs` |
| U2-R2#2 | TOCTOU in `write_file` path validation — path validated then file written non-atomically | `tools.rs` |
| U2-R2#3 | TOCTOU in `edit_file` between read and write — file may change between operations | `tools.rs` |

---

## Recommended Action Items

### 1. Operational correctness sandboxing (lot)

TOCTOU findings below have partial code mitigations possible (e.g., `O_NOFOLLOW`, `flock`), but mitigated by lot's per-phase process sandboxing.

| Ref | Summary |
|-----|---------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` |
| U2-R2#2 | TOCTOU in `write_file` path validation |
| U2-R2#3 | TOCTOU in `edit_file` between read and write |
| *(full)* | Per-phase access policy enforcement via lot process sandboxing. See [DESIGN.md](DESIGN.md#operational-correctness-lot). |
