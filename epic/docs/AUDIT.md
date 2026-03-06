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
| U14 | git | `git.rs` | git diff --numstat, scope circuit breaker support (stub removed) |
| U15 | metrics | `metrics.rs` | Token/cost tracking (stub removed) |
| U16 | services | `services/*.rs` | Stubs: document_store, research, verification (removed) |
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
| **U14** git | [x] | -- | [x] | [x] | -- | [x] | -- | -- |
| **U15** metrics | [x] | -- | [x] | [x] | -- | -- | -- | -- |
| **U16** services | -- | -- | -- | [x] | -- | -- | -- | -- |
| **U17** docs | -- | -- | -- | [x] | -- | -- | [x] | [x] |

Cross-cutting: X1 (Cargo.toml), X2 (clippy pedantic), X3 (compiler warnings), X4 (CI readiness), X5 (global patterns), X6 (constants vs config). Broad-lens: B1–B4 (simplification), B5–B8 (design).

---

## Audit Results Summary

**Completed:** 2026-03-05. All 95 review cells executed. 541 original findings.

**Post-audit remediation** addressed model selection, config wiring, task/recovery caps, retry persistence, checkpoint adjust/escalate, stale documentation, container setup documentation, CI pipeline, Flick dependency pinning, main.rs testability, and dead stub removal — resolving 62 findings fully and 15 partially.

### Current Findings by Severity

| Severity | Still Valid | Partially Resolved |
|----------|------------|-------------------|
| Critical | 0 | 0 |
| Major | 77 | 8 |
| Minor | 224 | 7 |
| Note | 164 | 3 |
| **Total** | **461** | **15** |

### Remaining Findings by Category

| Category | Approx count | Critical | Major |
|---|---|---|---|
| Operational correctness sandboxing (Frida, TOCTOU, per-phase enforcement) | ~33 | 0 | ~16 |
| Testability (no injection seams, zero coverage in init/TUI/main/state) | ~65 | 0 | ~15 |
| Simplification/dedup (retry loops, event variants, prompt boilerplate) | ~70 | 0 | ~10 |
| Error handling (inconsistent fatal vs best-effort, panics, silent swallowing) | ~45 | 0 | ~8 |
| Dead code/stubs (unused ToolGrant flags, usage tracking) | ~21 | 0 | ~3 |
| Design intent gaps (prompt content, tool grants, missing review phase) | ~40 | 0 | ~10 |
| Doc drift (TUI event names, CLI syntax, type mismatches) | ~25 | 0 | ~5 |
| Correctness (empty subtask validation, cycle detection, phase transitions) | ~30 | 0 | ~8 |
| Other (clippy, naming, notes) | ~128 | 0 | 0 |

---

## Critical Findings (all resolved)

All 4 original critical findings have been resolved: C1 (security isolation documentation — README + startup detection), C2 (operational correctness — reclassified as Major, tracked below), C3 (CI pipeline — GitHub Actions added), C4 (main.rs untestable — `run()` extracted).

---

## Major Findings (77 still valid, 8 partially resolved)

### Operational Correctness & Sandboxing

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R2#2 | Full environment inherited by bash child process — credentials, tokens, PATH all exposed | `tools.rs` |
| U5-R2#3 | No write-content size limit on `write_file` — agent can exhaust disk | `tools.rs` |
| U5-R2#4 | No regex pattern size/complexity limit in `tool_grep` — ReDoS vector | `tools.rs` |
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` — race between validation and open | `tools.rs` |
| U5-R1#2 | Bash timeout does not kill process group — grandchild processes orphaned | `tools.rs` |
| U5-R1#3 | Glob filter bypass when `strip_prefix` fails in `tool_grep` — files outside root may be searched | `tools.rs` |
| U2-R2#2 | TOCTOU in `write_file` path validation — path validated then file written non-atomically | `tools.rs` |
| U2-R2#3 | TOCTOU in `edit_file` between read and write — file may change between operations | `tools.rs` |
| U2-R2#4 | `credential_name` passed through JSON config with potential leakage in error paths | `flick.rs`, `config_gen.rs` |
| U1-R2#2 | `git diff` subprocess in `check_scope_circuit_breaker` has no `tokio::time::timeout` — can hang indefinitely | `orchestrator.rs` |

### Correctness

| Ref | Finding | Location |
|-----|---------|----------|
| U1-R1#1 | `execute_branch` can report Success when all children failed after recovery exhaustion | `orchestrator.rs` |
| U3-R1#1 | `DecompositionWire::try_from` accepts empty `subtasks` vec — creates branch with no children | `config_gen.rs` |
| U3-R1#2 | `RecoveryPlanWire::try_from` accepts empty `subtasks` vec — creates recovery with no work | `config_gen.rs` |
| U8-R1#1 | `load()` does not validate `next_id > max(existing task IDs)` — ID collision on resume | `state.rs` |
| U8-R1#2 | `dfs_order` has no cycle detection — infinite loop on corrupted state files | `state.rs` |
| U5-R3#1 | Bash timeout doesn't explicitly kill child/process group — resources leak | `tools.rs` |
| U5-R3#2 | `tool_bash` returns `Ok` for non-zero exit status — callers may misinterpret failures | `tools.rs` |
| U2-R1#1 | `run_structured` does not guard against `ToolCallsPending` status from Flick | `flick.rs` |
| U7-R3#1 | `Task::path` and `current_model` are `Option` with no safe accessor — callers unwrap unsafely | `task/mod.rs` |

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
| U14-R6 | git module empty; scope check hardwired in orchestrator with no trait boundary (multiple findings) | `git.rs`, `orchestrator.rs` |

### Simplification & Deduplication

| Ref | Finding | Location |
|-----|---------|----------|
| U1-R5#1, B1#1, B5#1 | `execute_leaf` and `leaf_fix_loop` share ~120 lines of identical retry-escalation state machine | `orchestrator.rs` |
| U2-R5#2 | `execute_leaf` and `fix_leaf` in FlickAgent have identical bodies after prompt line | `flick.rs` |
| U2-R5#3 | `design_and_decompose` and `design_fix_subtasks` in FlickAgent share identical tail | `flick.rs` |
| U3-R5#1 | Subtask schema duplicated between `decomposition_schema` and `recovery_plan_schema` | `config_gen.rs` |
| U3-R6#1 | `build_config` monolith couples JSON assembly to `flick::Config::from_str` | `config_gen.rs` |
| B3#1 | `VerificationStarted`/`VerificationComplete` event pair — redundant with `TaskCompleted`/`TaskFailed` | `events.rs`, `orchestrator.rs` |
| B3#2 | `SubtasksCreated` emitted redundantly alongside `RecoverySubtasksCreated`/`FixSubtasksCreated` | `events.rs`, `orchestrator.rs` |

### Error Handling

| Ref | Finding | Location |
|-----|---------|----------|
| B4#1 | `verify()` errors are fatal inside fix loops — should be best-effort like recovery | `orchestrator.rs` |
| B4#2 | `design_fix_subtasks` errors are fatal — inconsistent with best-effort `design_recovery_subtasks` | `orchestrator.rs` |
| U11-R1#1 | `read_line()` in init silently discards I/O errors | `init.rs` |
| U12-R1#1 | TUI abort path does not save state — user loses progress on Ctrl-C during TUI mode | `main.rs` |
| U5-R3#1 | Bash timeout doesn't explicitly kill child — resources leak on timeout (also correctness) | `tools.rs` |

### Dead Code & Stubs

| Ref | Finding | Location |
|-----|---------|----------|
| U2-R7#5 | No usage/cost tracking — `result.usage` never read in production code | `flick.rs` |

*Resolved: U14-R4#1 (`git.rs`), U15-R4#1 (`metrics.rs`), U16-R4#1 (`services/*.rs`), X5#1/X5#2 (mod declarations) — all removed 2026-03-06.*

### Design Intent

| Ref | Finding | Location |
|-----|---------|----------|
| U2-R7#4 | Decompose tool grant is READ-only — AGENT_DESIGN.md specifies EXPLORE (READ\|EXECUTE\|WEB) | `flick.rs` |
| U4-R7#1 | No cost/scope guardrails in any prompt — agents have no budget awareness | `prompts.rs` |
| U4-R7#2 | Assessment prompt omits tie-breaking bias toward branch (EPIC_DESIGN2 specifies prefer-branch) | `prompts.rs` |
| U4-R7#3 | Assessment prompt omits root-is-always-branch rule | `prompts.rs` |
| U7-R7#3 | File-level review and simplification review phases not implemented | `orchestrator.rs` |
| B2#1 | `assess` config includes tool definitions but `run_structured` ignores tool calls | `flick.rs`, `config_gen.rs` |
| B8#1 | Checkpoint agent cannot see child subtasks — classifies without knowing the plan structure | `orchestrator.rs`, `prompts.rs` |
| B8#2 | Decomposition rationale promised in prompt but not delivered to recovery agent | `prompts.rs` |
| B7#1 | Leaf fix loop runs unchecked on fix subtasks — no guard prevents recursive fix-loop-within-fix-loop | `orchestrator.rs` |

### Documentation Drift

| Ref | Finding | Location |
|-----|---------|----------|
| U17-R8#9 | VERIFICATION.md `timeout_secs: u64` vs code `timeout: u32` | `VERIFICATION.md` |
| U17-R8#10 | TASK_MODEL.md references `submit_result` tool — no such tool exists | `TASK_MODEL.md` |
| U17-R8#11 | Assessment uses `run_structured` (no tools), but AGENT_DESIGN.md says READ tools | `AGENT_DESIGN.md` |
| U17-R8#14 | TUI_DESIGN.md event names don't match actual Event enum variants | `TUI_DESIGN.md` |
| U17-R8#15 | TUI_DESIGN.md `VerificationResult` vs actual `VerificationComplete` | `TUI_DESIGN.md` |

### Partially Resolved Majors

| Ref | Finding | Status |
|-----|---------|--------|
| U1-R7#5 | `build_context` missing `parent_decomposition_rationale` and `parent_discoveries` | Decomposition rationale stored on Task but not propagated to context |
| U4-R7#4 | `verify()` prompt not split into leaf vs branch variants | verify() now accepts `model: Model` param, but prompt text is still identical |
| U6-R1#1 | `assess()` calls `run_structured` with tools config that won't be handled | Model corrected to Haiku, but tool config still passed to non-tool runner |
| U10-R1#2 | `LimitsConfig` values used but no comprehensive validation | Some fields clamped to min 1, but no full validate() method |
| U10-R6#2 | Config validation incomplete | Clamping added for some fields; no comprehensive boundary checks |
| U10-R6#3 | No dedicated config `load()` with filesystem abstraction | Config loaded in main.rs directly; no config-module-level load function |
| U17-R8#6 | CONFIGURATION.md CLI section still shows old syntax | Actual subcommands are run/resume/status/init; doc shows `epic "problem"`, `epic --resume` |
| B7#2 | Recovery subtasks get fresh budgets enabling cost growth | `max_total_tasks` cap provides a global safeguard; recovery depth inherited; but per-branch budgets still reset |

---

## Recommended Action Items (Priority Order)

1. **Operational correctness sandboxing (Frida).** Per-phase access policy enforcement via runtime interception. See [SANDBOXING.md](SANDBOXING.md) Concern 2. Complex, multiple open questions — start with prototype.
2. **Deduplicate retry/escalation loop.** Extract shared state machine from `execute_leaf` and `leaf_fix_loop`.
3. **Add cycle detection to `dfs_order`.** Infinite loop on corrupted state files.
4. **Fix error handling consistency in fix loops.** Make `verify()` and `design_fix_subtasks` errors best-effort within fix loops, matching recovery pattern.
5. **Add empty-subtask validation.** `DecompositionWire` and `RecoveryPlanWire` should reject empty subtask lists.
6. **Kill process group on bash timeout.** Current code only kills the direct child, orphaning grandchildren.
