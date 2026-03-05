# Project Audit Plan

## Purpose

Full audit of the Epic codebase at v1 completion (110 tests, ~9,400 lines of Rust across 29 source files). Goals: identify correctness issues, stale/dead code, doc drift, security gaps, design fidelity, and simplification opportunities before real-world use.

## Approach

The audit is organized as a **review matrix**: review types on one axis, code units on the other. Each cell is a focused task for a single agent. Not every cell applies — cells marked `--` are intentionally skipped.

## Status Key

- `[ ]` — Not started
- `[~]` — In progress
- `[x]` — Complete
- `[!]` — Blocked or needs discussion
- `--` — Not applicable (intentionally skipped)

---

## Code Units

| ID | Unit | Files | Lines | Description |
|----|------|-------|-------|-------------|
| U1 | orchestrator | `orchestrator.rs` | 4,512 | DFS loop, retry/escalation, fix loops, recovery, checkpoints, resume |
| U2 | agent/flick | `agent/flick.rs` | 533 | FlickClient wrapper, tool loop, structured output, resume |
| U3 | agent/config_gen | `agent/config_gen.rs` | 818 | Wire types, TryFrom conversions, JSON schemas |
| U4 | agent/prompts | `agent/prompts.rs` | 495 | Prompt assembly for all agent methods |
| U5 | agent/tools | `agent/tools.rs` | 932 | 6 tool implementations, path sandboxing, size limits |
| U6 | agent/models | `agent/models.rs`, `agent/mod.rs` | 147 | Model IDs, token limits, AgentService trait |
| U7 | task | `task/*.rs` | 208 | Task struct, phases, Magnitude, LeafResult, subtypes |
| U8 | state | `state.rs` | 138 | EpicState persistence, atomic writes, load/save |
| U9 | events | `events.rs` | ~100 | Event enum, all variants |
| U10 | config | `config/*.rs` | 153 | EpicConfig, VerificationStep, TOML loading |
| U11 | init | `init.rs` | 324 | Agent-driven interactive scaffolding |
| U12 | cli + main | `cli.rs`, `main.rs` | ~350 | Clap CLI, wiring, TUI/headless split, shutdown |
| U13 | tui | `tui/*.rs` | 766 | TuiApp, task tree, worklog, metrics panels |
| U14 | git | `git.rs` | ~80 | git diff --numstat, scope circuit breaker support |
| U15 | metrics | `metrics.rs` | ~60 | Token/cost tracking |
| U16 | services | `services/*.rs` | 5 | Stubs: document_store, research, verification |
| U17 | docs | `docs/*.md` | — | All 13 design documents |

## Review Types

| ID | Type | Focus |
|----|------|-------|
| R1 | **Correctness** | Logic errors, edge cases, unsound assumptions, off-by-one, missed error paths |
| R2 | **Security** | Injection, sandboxing, TOCTOU, credential exposure, resource exhaustion |
| R3 | **Error handling** | Panics (unwrap/expect), error propagation, graceful degradation, resource cleanup |
| R4 | **Dead code & cruft** | Unused code, stubs, stale comments, subprocess/ZeroClaw/YAML remnants, stale allow annotations |
| R5 | **Simplification** | Unnecessary complexity, duplicated logic, boilerplate reduction, extraction opportunities |
| R6 | **Testability** | Mock boundaries, test friction, isolation, missing coverage, state machine clarity |
| R7 | **Design intent** | Fidelity to EPIC_DESIGN2, agent autonomy balance, cost control, verification enforcement |
| R8 | **Doc consistency** | Design doc matches implementation, stale references, missing updates |

---

## Review Matrix

Each cell is one focused agent task. Cells contain status checkboxes.

| Unit | R1 Correct | R2 Security | R3 Errors | R4 Cruft | R5 Simplify | R6 Tests | R7 Design | R8 Docs |
|------|-----------|-------------|-----------|----------|-------------|----------|-----------|---------|
| **U1** orchestrator | [x] | [x] | [x] | [x] | [x] | [x] | [x] | -- |
| **U2** agent/flick | [x] | [x] | [x] | [x] | [x] | [x] | [x] | -- |
| **U3** agent/config_gen | [x] | -- | [x] | [x] | [x] | [x] | -- | -- |
| **U4** agent/prompts | [x] | -- | [x] | [x] | [x] | [x] | [x] | -- |
| **U5** agent/tools | [x] | [x] | [x] | [x] | [x] | [x] | -- | -- |
| **U6** agent/models | [x] | -- | [x] | [x] | -- | [x] | [x] | -- |
| **U7** task | [x] | -- | [x] | [x] | [x] | [x] | [x] | -- |
| **U8** state | [x] | [x] | [x] | [x] | -- | [x] | -- | -- |
| **U9** events | [x] | -- | -- | [x] | [x] | -- | -- | -- |
| **U10** config | [x] | -- | [x] | [x] | -- | [x] | -- | -- |
| **U11** init | [x] | -- | [x] | [x] | [x] | [x] | -- | -- |
| **U12** cli + main | [x] | [x] | [x] | [x] | [x] | [x] | -- | -- |
| **U13** tui | [x] | -- | [x] | [x] | [x] | [x] | -- | -- |
| **U14** git | [x] | -- | [x] | [x] | -- | [x] | -- | -- |
| **U15** metrics | [x] | -- | [x] | [x] | -- | -- | -- | -- |
| **U16** services | -- | -- | -- | [x] | -- | -- | -- | -- |
| **U17** docs | -- | -- | -- | [x] | -- | -- | [x] | [x] |

### Cell count: 81 matrix cells

---

## Cross-Cutting Reviews

Some concerns span the entire codebase and don't fit neatly into one unit. These are separate single-agent tasks.

- [x] **X1: Cargo.toml & dependencies** — Unused deps, feature flags, edition, metadata, build reproducibility.
- [x] **X2: Clippy pedantic** — Run `cargo clippy -- -W clippy::pedantic` across the whole codebase. Triage warnings.
- [x] **X3: Compiler warnings** — `cargo build` with all warnings enabled. Check for suppressed warnings.
- [x] **X4: CI readiness** — Assess what a CI pipeline would need. No CI config exists today.
- [x] **X5: Global patterns** — Naming consistency, visibility boundaries, module organization across the whole project.
- [x] **X6: Constants vs. config** — Audit all hardcoded constants (RETRIES_PER_TIER, MAX_RECOVERY_ROUNDS, etc.). Which should be configurable?

---

## Broad-Lens Reviews

These reviews deliberately span multiple code units to find issues that are **invisible when examining a single module in isolation**. Each agent reads all files in its scope. Agents must NOT raise issues that could be found within a single code unit — those belong in the matrix cells above.

### Simplification (broad)

- [x] **B1: Duplicated patterns across modules** — Read all of: `orchestrator.rs`, `agent/flick.rs`, `agent/config_gen.rs`, `init.rs`. Identify repeated patterns that appear in 2+ modules: retry/escalation progressions, agent-call-then-parse sequences, checkpoint-save-after-mutation patterns, error-to-string formatting. Only flag patterns where a shared abstraction would reduce total code without adding indirection that hurts readability.

- [x] **B2: Data flow and type conversions** — Read: `agent/config_gen.rs` (wire types), `task/mod.rs` (domain types), `agent/prompts.rs` (context formatting), `orchestrator.rs` (where conversions happen). Trace the full path: domain type → prompt context → agent call → wire type → TryFrom → domain type. Identify unnecessary intermediate representations, fields that are serialized but never read by agents, conversions that could be eliminated by aligning types.

- [x] **B3: Event and state coupling** — Read: `events.rs`, `orchestrator.rs`, `tui/mod.rs`, `state.rs`. Events serve two consumers (TUI display and implicit state logging). Are there events that exist only because the TUI needs them but add checkpoint/persistence overhead? Are there state changes that should emit events but don't? Is the event enum pulling its weight or should some variants be consolidated?

- [x] **B4: Error handling uniformity** — Read all agent-calling code in: `orchestrator.rs`, `agent/flick.rs`, `init.rs`. Compare how agent errors are handled across every call site. Are some calls best-effort (log and continue) while similar calls are fatal? Is the inconsistency intentional or accidental? Could a common error handling strategy reduce code without losing nuance?

### Design (broad)

- [x] **B5: Orchestrator responsibility boundaries** — Read: `orchestrator.rs`, `agent/mod.rs` (trait), `task/mod.rs`, `state.rs`. The orchestrator is 4,512 lines and owns: task execution, state mutation, checkpointing, fix loops, recovery, event emission, and agent dispatch. Map which responsibilities are inherent to orchestration vs. which are bolted on. Identify responsibilities that could live closer to the data they operate on (e.g., should `Task` own its phase transitions? Should fix-loop logic live with the agent layer?). Do NOT recommend extraction for its own sake — only where the current placement causes confusion, duplication, or coupling.

- [x] **B6: Agent contract coherence** — Read: `agent/mod.rs` (AgentService trait), `agent/flick.rs` (implementation), `agent/config_gen.rs` (wire types), `agent/prompts.rs` (prompt assembly). The AgentService trait has ~8 methods. Do they form a coherent contract, or have some methods been added ad-hoc as features were bolted on? Are there methods with nearly identical signatures that could be unified? Do the wire types and prompt structures follow a consistent pattern, or has each method evolved its own conventions?

- [x] **B7: Recovery and fix architecture** — Read: the fix loop sections of `orchestrator.rs`, the recovery sections of `orchestrator.rs`, `agent/mod.rs` (recovery + fix trait methods). The system has 5 layers of failure handling: retry at same tier, model escalation, leaf fix loop, branch fix loop, recovery re-decomposition. Review how these layers compose. Can a task go through all 5 layers in a single run? Are there interaction edge cases (e.g., a recovery subtask entering a fix loop that triggers another recovery)? Are the guard rails (is_fix_task, max rounds) sufficient and consistently applied?

- [x] **B8: Prompt and context pipeline** — Read: `agent/prompts.rs`, `agent/config_gen.rs` (schemas), `orchestrator.rs` (context building: `build_context`, sibling summaries, checkpoint guidance). The quality of agent output depends entirely on what goes into the prompt. Trace the full context pipeline for each agent method. Is context complete (does the agent have what it needs)? Is it minimal (no noise that wastes tokens)? Are sibling discoveries, checkpoint guidance, and failure reasons always included when relevant and omitted when not?

---

## Agent Instructions

Each agent receives:
1. The **review type definition** (from the Review Types table above).
2. The **code unit files** to read.
3. For R7 (Design intent): also read `EPIC_DESIGN2.md` and relevant design docs.
4. For R8 (Doc consistency): read the design doc and the corresponding source files.

Each agent produces:
- A list of **findings**, each with: severity (critical/major/minor/note), description, file:line reference where applicable, and suggested fix if obvious.
- Findings are recorded in the matrix section below.

---

## Findings

Findings are recorded per cell after each agent completes. Format:

### U{n}-R{n}: {unit} / {review type}

**Status:** [ ] Not started

| # | Severity | Finding | Location | Suggestion |
|---|----------|---------|----------|------------|
| | | | | |

---

## Execution Order

Recommended priority (highest-value cells first):

**Phase 1 — High-risk, high-complexity units:**
- U1 (orchestrator): R1, R3, R5, R6, R7 — largest file, most complex logic
- U5 (tools): R1, R2 — security-critical
- U2 (flick): R1, R3 — external API boundary

**Phase 2 — Data integrity and agent behavior:**
- U3 (config_gen): R1, R5 — wire type correctness, boilerplate
- U4 (prompts): R1, R7 — prompt quality drives agent effectiveness
- U8 (state): R1, R2 — persistence correctness
- U7 (task): R1, R7 — core data model

**Phase 3 — Supporting modules:**
- U12 (cli+main): R1, R3 — entry point, shutdown
- U11 (init): R1, R3 — user-facing interactive flow
- U13 (tui): R1, R3 — display correctness, crash recovery

**Phase 4 — Cleanup and docs:**
- All remaining R4 (cruft) cells
- U17 (docs): R8, R7
- Cross-cutting reviews X1–X6

**Phase 5 — Broad-lens reviews (after matrix cells complete):**
- B1–B4 (simplification broad) — depend on matrix findings to avoid re-raising known issues
- B5–B8 (design broad) — depend on R7 matrix cells for per-unit design context

---

## Summary

| Review Type | Cells | Done | Findings |
|-------------|-------|------|----------|
| R1 Correctness | 15 | 15 | 78 |
| R2 Security | 5 | 5 | 35 |
| R3 Error handling | 14 | 14 | 51 |
| R4 Dead code & cruft | 17 | 17 | 45 |
| R5 Simplification | 10 | 10 | 63 |
| R6 Testability | 13 | 13 | 96 |
| R7 Design intent | 6 | 6 | 57 |
| R8 Doc consistency | 1 | 1 | 20 |
| Cross-cutting | 6 | 6 | 57 |
| Broad-lens simplification | 4 | 4 | 17 |
| Broad-lens design | 4 | 4 | 22 |
| **Total** | **95** | **95** | **541** |

---

## Audit Results Summary

**Completed:** 2026-03-05. All 95 review cells executed by independent agents. Detailed findings in `docs/audit/*.md`.

### Findings by Severity

| Severity | Count |
|----------|-------|
| Critical | 4 |
| Major | 120 |
| Minor | 241 |
| Note | 176 |
| **Total** | **541** |

### Top 5 Most Concerning Findings

**1. Unsandboxed bash execution (U5-R2 #1, U2-R2 #1) — CRITICAL**
`tool_bash` passes LLM-supplied commands to `sh -c` with zero sandboxing: no containers, namespaces, network isolation, or command allowlist. An agent can exfiltrate data, install software, access credentials, or destroy the host. The `project_root` cwd is trivially escaped. This is the single highest-risk issue in the codebase.

**2. ~~Config not loaded at runtime (U10-R1 #1, U7-R1 #1, X6 #1-3) — MAJOR~~ RESOLVED**
~~`epic.toml` configuration is collected via `epic init` and persisted, but the orchestrator never reads it. `MAX_DEPTH`, `RETRIES_PER_TIER`, `MAX_RECOVERY_ROUNDS`, and model preferences are all hardcoded constants. User customization is silently ignored.~~ Config now loaded at startup. All hardcoded constants replaced with `LimitsConfig` fields. Model preferences wired through `ModelConfig`. Verification steps included in prompts.

**3. Model selection diverges from design spec (U1-R7, U2-R7, U6-R7, U4-R7) — MAJOR**
Assessment uses Sonnet instead of Haiku (overspend on classification). Decomposition ignores the assessment-selected model. Recovery assessment uses Sonnet instead of Opus (under-quality for critical decision). Verification always uses Sonnet instead of `max(Haiku, impl_model)`. These affect both cost control and agent effectiveness.

**4. Recovery subtasks get fresh budgets — multiplicative cost risk (B7 #2) — MAJOR** *(Resolved)*
Recovery subtasks are created with `is_fix_task: false` and inherit the parent's `recovery_rounds` counter, preventing fresh recovery budgets. Combined with a global `max_total_tasks` cap (default 100, configurable via `epic.toml`) and configurable `max_depth` (default 8), multiplicative recovery depth is now bounded. *(Mitigations applied: recovery subtasks inherit the parent's `recovery_rounds`, and a global `max_total_tasks` cap prevents unbounded task creation.)*

**5. Documentation drift from Flick library migration (U17-R4, U17-R8, U17-R7) — MAJOR**
ARCHITECTURE.md, CONFIGURATION.md, and DOCUMENT_STORE.md still describe Flick as a subprocess/external executable. AGENT_DESIGN.md has incorrect model assignments. CONFIGURATION.md documents a removed `flick_path` option and a non-existent CLI interface. 14+ stale references across docs.

### Recommended Action Items (Priority Order)

1. **Sandbox the bash tool.** Run agent commands in a container, bubblewrap, or restricted shell. Drop network access by default. This is a security prerequisite for real-world use.

2. **Wire epic.toml to the orchestrator.** Load config at startup, pass limits to `Orchestrator::new`. Remove or deprecate hardcoded constants. The config structs and init flow already exist — only the plumbing is missing.

3. **Fix model selection to match AGENT_DESIGN.md.** Assessment → Haiku, decomposition → assessment-selected model, recovery assessment → Opus, verification → max(Haiku, impl_model). Or update the design doc if the current choices are intentional.

4. ~~**Cap total task count and recovery depth.**~~ **Done.** Global `max_total_tasks` cap (default 100) added, recovery subtasks inherit parent's `recovery_rounds`.

5. **Update stale documentation.** Remove subprocess references from ARCHITECTURE.md, fix CLI description in CONFIGURATION.md, remove `flick_path`, update model assignments in AGENT_DESIGN.md.

6. **Add CI pipeline.** GitHub Actions with `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`. Pin the Flick git dependency to a rev/tag. Add `rust-toolchain.toml`.

7. **Extract main() into testable function.** Replace `process::exit` calls with `bail!`, extract `async fn run()` that accepts injected dependencies. This unlocks integration testing for the entire entry point.

8. **Remove dead modules.** `src/git.rs`, `src/metrics.rs`, `src/services/*.rs` are empty stubs declared in `main.rs` but never used.

9. **Deduplicate retry/escalation loop.** `execute_leaf` and `leaf_fix_loop` share ~120 lines of identical state machine code. Extract a shared `retry_with_escalation` helper.

10. **Add cycle detection to `dfs_order`.** `EpicState::dfs_order` will infinite-loop on cyclic task graphs from corrupted state files.
