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

**Post-audit remediation** addressed model selection, config wiring, task/recovery caps, retry persistence, checkpoint adjust/escalate, stale documentation, container setup documentation, CI pipeline, Flick dependency pinning, main.rs testability, dead stub removal, retry/escalation deduplication, empty-subtask validation, and bash process group kill — resolving 70 findings fully and 15 partially.

### Current Findings by Severity

| Severity | Still Valid | Partially Resolved |
|----------|------------|-------------------|
| Critical | 0 | 0 |
| Major | 70 | 8 |
| Minor | 223 | 7 |
| Note | 164 | 3 |
| **Total** | **459** | **15** |

### Remaining Findings by Category

| Category | Approx count | Critical | Major |
|---|---|---|---|
| Operational correctness sandboxing (Frida, TOCTOU, per-phase enforcement) | ~33 | 0 | ~16 |
| Testability (no injection seams, zero coverage in init/TUI/main/state) | ~65 | 0 | ~15 |
| Simplification/dedup (retry loops, event variants, prompt boilerplate) | ~67 | 0 | ~9 |
| Error handling (inconsistent fatal vs best-effort, panics, silent swallowing) | ~42 | 0 | ~5 |
| Dead code/stubs (unused ToolGrant flags, usage tracking) | ~21 | 0 | ~3 |
| Design intent gaps (prompt content, tool grants, missing review phase) | ~40 | 0 | ~10 |
| Doc drift (TUI event names, CLI syntax, type mismatches) | ~25 | 0 | ~5 |
| Correctness (cycle detection, phase transitions) | ~27 | 0 | ~5 |
| Other (clippy, naming, notes) | ~128 | 0 | 0 |

---

## Critical Findings (all resolved)

All 4 original critical findings have been resolved: C1 (security isolation documentation — README + startup detection), C2 (operational correctness — reclassified as Major, tracked below), C3 (CI pipeline — GitHub Actions added), C4 (main.rs untestable — `run()` extracted).

---

## Major Findings (70 still valid, 8 partially resolved)

### Operational Correctness & Sandboxing

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R2#2 | Full environment inherited by bash child process — credentials, tokens, PATH all exposed | `tools.rs` |
| U5-R2#3 | No write-content size limit on `write_file` — agent can exhaust disk | `tools.rs` |
| U5-R2#4 | No regex pattern size/complexity limit in `tool_grep` — ReDoS vector | `tools.rs` |
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` — race between validation and open | `tools.rs` |
| U5-R1#3 | Glob filter bypass when `strip_prefix` fails in `tool_grep` — files outside root may be searched | `tools.rs` |
| U2-R2#2 | TOCTOU in `write_file` path validation — path validated then file written non-atomically | `tools.rs` |
| U2-R2#3 | TOCTOU in `edit_file` between read and write — file may change between operations | `tools.rs` |
| U2-R2#4 | `credential_name` passed through JSON config with potential leakage in error paths | `flick.rs`, `config_gen.rs` |
| U1-R2#2 | `git diff` subprocess in `check_scope_circuit_breaker` has no `tokio::time::timeout` — can hang indefinitely | `orchestrator.rs` |

### Correctness

| Ref | Finding | Location |
|-----|---------|----------|
| U1-R1#1 | `execute_branch` can report Success when all children failed after recovery exhaustion | `orchestrator.rs` |
| U8-R1#1 | `load()` does not validate `next_id > max(existing task IDs)` — ID collision on resume | `state.rs` |
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
| U2-R5#2 | `execute_leaf` and `fix_leaf` in FlickAgent have identical bodies after prompt line | `flick.rs` |
| U2-R5#3 | `design_and_decompose` and `design_fix_subtasks` in FlickAgent share identical tail | `flick.rs` |
| U3-R5#1 | Subtask schema duplicated between `decomposition_schema` and `recovery_plan_schema` | `config_gen.rs` |
| U3-R6#1 | `build_config` monolith couples JSON assembly to `flick::Config::from_str` | `config_gen.rs` |
| B3#1 | `VerificationStarted`/`VerificationComplete` event pair — redundant with `TaskCompleted`/`TaskFailed` | `events.rs`, `orchestrator.rs` |
| B3#2 | `SubtasksCreated` emitted redundantly alongside `RecoverySubtasksCreated`/`FixSubtasksCreated` | `events.rs`, `orchestrator.rs` |

*Resolved: U1-R5#1/B1#1/B5#1 (retry loop dedup) — extracted `leaf_retry_loop` 2026-03-06.*

### Error Handling

| Ref | Finding | Location |
|-----|---------|----------|
| U11-R1#1 | `read_line()` in init silently discards I/O errors | `init.rs` |
| U12-R1#1 | TUI abort path does not save state — user loses progress on Ctrl-C during TUI mode | `main.rs` |
*Resolved: U5-R3#1 (bash process group kill), B4#1, B4#2 — all resolved 2026-03-06.*

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

*Resolved: former #1 (empty-subtask validation), former #2 (bash process group kill) — 2026-03-06.*

### 1. Correctness fixes (5 majors)

Logic errors that can produce wrong outcomes at runtime.

| Ref | Summary | Fix |
|-----|---------|-----|
| U1-R1#1 | `execute_branch` reports Success when all children failed after recovery exhaustion | Check final child statuses after recovery rounds |
| U8-R1#1 | `load()` doesn't validate `next_id > max(existing IDs)` — ID collision on resume | Validate and bump `next_id` in `load()` |
| U5-R3#2 | `tool_bash` returns `Ok` for non-zero exit — callers may misinterpret | Return error or distinct result for non-zero exit |
| U2-R1#1 | `run_structured` doesn't guard against `ToolCallsPending` status from Flick | Handle or reject `ToolCallsPending` explicitly |
| U7-R3#1 | `Task::path` and `current_model` are `Option` with no safe accessor — callers unwrap | Add accessor methods that return `Result` or panic-free defaults |

### 2. Input validation & resource limits (6 majors)

Prevent resource exhaustion and data leakage — all fixable with standard code changes.

| Ref | Summary | Fix |
|-----|---------|-----|
| U5-R2#2 | Full environment inherited by bash child — credentials exposed | `Command::env_clear()` + explicit allowlist |
| U5-R2#3 | No write-content size limit on `write_file` — disk exhaustion | Add `MAX_WRITE_BYTES` constant and reject oversized content |
| U5-R2#4 | No regex complexity limit in `tool_grep` — ReDoS vector | Use `RegexBuilder::size_limit()` / `dfa_size_limit()` |
| U5-R1#3 | Glob filter bypass when `strip_prefix` fails in `tool_grep` | Change `strip_prefix` failure from implicit-pass to explicit-skip |
| U1-R2#2 | `git diff` subprocess has no timeout — can hang indefinitely | Wrap in `tokio::time::timeout()` |
| U2-R2#4 | `credential_name` passed through JSON with potential leakage in error paths | Redact credential names in error/log output |

### 3. Design intent alignment (9 majors + 4 partially resolved)

Divergences from EPIC_DESIGN2.md and AGENT_DESIGN.md specs.

| Ref | Summary | Fix |
|-----|---------|-----|
| U2-R7#4 | Decompose tool grant is READ-only — spec says EXPLORE (READ\|EXECUTE\|WEB) | Update tool grant to match AGENT_DESIGN.md |
| U4-R7#1 | No cost/scope guardrails in any prompt | Add budget/scope awareness to prompts |
| U4-R7#2 | Assessment prompt omits tie-breaking bias toward branch | Add prefer-branch instruction per EPIC_DESIGN2 |
| U4-R7#3 | Assessment prompt omits root-is-always-branch rule | Add root-is-branch rule to assessment prompt |
| U7-R7#3 | File-level review and simplification review phases not implemented | Implement or explicitly defer with rationale |
| B2#1 | `assess` config includes tool definitions but `run_structured` ignores tool calls | Remove tool config from assess, or switch to `run_with_tools` |
| B8#1 | Checkpoint agent cannot see child subtasks | Include child task list in checkpoint prompt context |
| B8#2 | Decomposition rationale not delivered to recovery agent | Thread rationale through to recovery prompt |
| B7#1 | Leaf fix loop runs unchecked on fix subtasks — recursive fix-within-fix | Add guard to skip fix loop for `is_fix_task` leaves |
| U1-R7#5 | *(partial)* `build_context` missing `parent_decomposition_rationale` and `parent_discoveries` | Add fields to `TaskContext`, populate in `build_context()` |
| U4-R7#4 | *(partial)* `verify()` prompt not split into leaf vs branch variants | Split `build_verify` into leaf/branch variants |
| U6-R1#1 | *(partial)* `assess()` passes tool config to `run_structured` which ignores it | Remove tool config from assess config |
| B7#2 | *(partial)* Recovery subtasks get fresh per-branch budgets | Propagate remaining budgets from parent |

### 4. Documentation drift (5 majors + 1 partially resolved)

Design docs out of sync with implementation.

| Ref | Summary | Fix |
|-----|---------|-----|
| U17-R8#9 | VERIFICATION.md `timeout_secs: u64` vs code `timeout: u32` | Update doc to match code type |
| U17-R8#10 | TASK_MODEL.md references `submit_result` tool — no such tool | Remove or replace reference |
| U17-R8#11 | Assessment uses `run_structured` (no tools), but AGENT_DESIGN.md says READ tools | Update doc to reflect actual no-tools approach |
| U17-R8#14 | TUI_DESIGN.md event names don't match actual Event enum variants | Update event names in doc |
| U17-R8#15 | TUI_DESIGN.md `VerificationResult` vs actual `VerificationComplete` | Update doc |
| U17-R8#6 | *(partial)* CONFIGURATION.md CLI section still shows old syntax | Rewrite CLI section to match `run`/`resume`/`status`/`init` |

### 5. Error handling (2 majors)

| Ref | Summary | Fix |
|-----|---------|-----|
| U11-R1#1 | `read_line()` in init silently discards I/O errors | Propagate or log the error |
| U12-R1#1 | TUI abort path does not save state — user loses progress on Ctrl-C | Save state in TUI shutdown handler |

### 6. Simplification (6 majors + 1 dead code)

Reduce duplication and remove unused code.

| Ref | Summary | Fix |
|-----|---------|-----|
| U2-R5#2 | `execute_leaf` and `fix_leaf` in FlickAgent have identical bodies | Extract shared implementation |
| U2-R5#3 | `design_and_decompose` and `design_fix_subtasks` share identical tail | Extract shared implementation |
| U3-R5#1 | Subtask schema duplicated between decomposition and recovery | Extract shared schema builder |
| U3-R6#1 | `build_config` monolith couples JSON assembly to `flick::Config::from_str` | Split JSON assembly from parsing |
| B3#1 | `VerificationStarted`/`VerificationComplete` events redundant with task events | Remove or merge |
| B3#2 | `SubtasksCreated` emitted redundantly alongside variant-specific events | Remove or merge |
| U2-R7#5 | No usage/cost tracking — `result.usage` never read | Remove dead field or implement tracking |

### 7. Config validation (3 partially resolved)

| Ref | Summary | Fix |
|-----|---------|-----|
| U10-R1#2 | `LimitsConfig` has no comprehensive validation | Add `validate()` with bounds checking |
| U10-R6#2 | Config validation incomplete — no boundary tests | Add `PartialEq` derives and boundary tests |
| U10-R6#3 | No dedicated config `load()` abstraction | Add `EpicConfig::load(path)` in config module |

### 8. Testability (16 majors)

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

### 9. Operational correctness sandboxing (Frida)

TOCTOU findings below have partial code mitigations possible (e.g., `O_NOFOLLOW`, `flock`), but full resolution requires Frida's per-phase syscall enforcement. Deferred until items 1–8 are addressed.

| Ref | Summary |
|-----|---------|
| U5-R1#1 | TOCTOU symlink race in `safe_path` with `allow_new_file` |
| U2-R2#2 | TOCTOU in `write_file` path validation |
| U2-R2#3 | TOCTOU in `edit_file` between read and write |
| *(full)* | Per-phase access policy enforcement via runtime interception. See [SANDBOXING.md](SANDBOXING.md) Concern 2. |
