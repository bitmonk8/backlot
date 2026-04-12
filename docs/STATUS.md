# Backlot — Status

## Flick

**Phase:** Complete. 347 tests passing, zero clippy errors. On reqwest 0.13.2.

**Implemented:**
- Monadic LLM call architecture (single model call per invocation, JSON result to stdout)
- Messages API (Anthropic) and Chat Completions (OpenAI-compatible) providers
- Two-step structured output for Chat Completions (tools + output_schema)
- Named models — `ProviderRegistry` (~/.flick/providers) and `ModelRegistry` (~/.flick/models), TOML-based, encrypted keys
- `RequestConfig` with builder pattern, `FlickClient` with model->provider resolution
- Cross-registry validation, config validation (deny_unknown_fields, mutual exclusion checks)
- Prompt caching — 2-breakpoint strategy, `CacheRetention` enum (None/Short/Long)
- Cache-aware cost computation, per-call timing, structured output cleaning
- CLI: `provider add/list`, `model add/list/remove`, `init`, `run`
- CLI input hardening (stdin cap, key validation, whitespace rejection)

**Next Work:** None identified.

---

## Lot

**Phase:** Implementation and audit remediation complete. 278 tests passing, CI green on Linux, macOS, Windows.

**Implemented:**
- Cross-platform process sandboxing (Linux namespaces+seccomp, macOS Seatbelt, Windows AppContainer)
- Policy builder API, path grants, stdio management, timeout handling
- Full test suite running in parallel

**Issues:** 69 NIT-level findings, 0 MUST FIX, 0 NON-CRITICAL. Tracked in GitHub Issues.

**Next Work:** None identified.

---

## Reel

**Phase:** Core agent runtime and tooling complete. 297 tests passing. CI green on all platforms.

**Implemented:**
- Agent runtime with tool loop and resume
- 6 built-in tools (Read, Write, Edit, Glob, Grep, NuShell)
- NuShell sandbox via persistent `nu --mcp` process inside lot sandbox
- `ToolHandler` trait for custom tool dispatch
- `ToolGrant` bitflags (WRITE/TOOLS/NETWORK)
- CLI binary, build infrastructure, CI pipeline
- `RunResult` with `Usage` (tokens + cost), `TurnRecord` transcript, per-call API latency

**Not Implemented:**
- `ToolHandler` consumer — trait exists but no real consumer yet (epic's Research Service is first consumer)

**Next Work:** None identified.

---

## Vault

**Phase:** All four core operations and CLI implemented. Reel observability integrated.

**Implemented:**
- File-based document store with librarian agent (reel-backed)
- Storage layer: `raw/`, `derived/`, JSONL changelog, versioned documents, name validation
- Four operations: bootstrap, record, query, reorganize
- `DerivedProducer` / `QueryResponder` traits, `ReelLibrarian` production implementation
- CLI with YAML config, JSON output, `--verbose` transcripts
- Observability: `SessionMetadata`, `TranscriptTurn`, `TurnUsage` types
- 121 tests

**Next Work:** None identified.

---

## Cue

**Phase:** Complete. 7 tests passing, zero clippy errors.

**Implemented:**
- Generic recursive task orchestration framework
- `TaskNode` trait (28 methods: 8 read accessors, 6 decision methods, 6 mutations, 8 lifecycle)
- `TaskStore` trait (task creation, storage, lookup, cross-task queries, tree context building)
- `Orchestrator<S: TaskStore, T: EventEmitter<CueEvent>>` coordination loop (DFS traversal, resume, retry, escalation, fix loops, recovery)
- All orchestration protocol types (`TaskId`, `TaskPhase`, `TaskPath`, `Model`, `TaskOutcome`, etc.)
- `CueEvent` enum (10 orchestration event variants)
- `LimitsConfig` (depth, retry budget, fix rounds, recovery rounds, task cap)
- Depends only on `traits` crate (for `EventEmitter<E>`)

**Next Work:** None identified.

---

## Mech

**Phase:** Deliverable 11 complete. Transition evaluation, `set_context`/`set_workflow` side-effects, and imperative-mode function execution (`mech/src/exec/schedule.rs`). The scheduler finds the entry block, executes block → side effects → transitions → next block until a terminal block is reached. Guard evaluation errors are treated as false per §10.2. `set_context`/`set_workflow` expressions are evaluated atomically (all see pre-write state). Self-loops and backward edges are supported via `clear_block_output` on `ExecutionContext`. CEL `UInt`→`Int` normalization added to `Namespaces::to_context` to fix serde_json's non-negative integer representation. 187 tests passing (+20 in Deliverable 11), zero clippy warnings.

**Spec** (`docs/MECH_SPEC.md`):
- Standalone crate providing a declarative YAML-based workflow definition format (not a custom-grammar language). Depends on cue (TaskNode integration) and reel (agent execution).
- All 12 sections drafted and reviewed. Covers: motivation, design goals, unified CDFG model, conversation model (history scoping, compaction, agent configuration), block specification (prompt + call blocks, field validity), transitions & guards (CEL, ordered evaluation, self-loops), template expressions & scoping (5 namespaces, CEL everywhere), schema handling (inline + `$ref` + workflow-level shared schemas), context & state (two-level declared variables, `set_context`/`set_workflow`), validation & error handling (24+ load-time checks, 5 runtime error types), cue integration (function = leaf task, model escalation interaction), YAML reference schema with complete worked example.
- Design decisions resolved during review:
  - **Per-call input on call blocks** — `call` accepts three forms: single string, uniform list (shared `input`), per-call list (`{ fn, input }` objects for heterogeneous function signatures).
  - **Call block output mapping** — optional `output` field on call blocks constructs the block's output from called functions' results (symmetric with `input` mapping).
  - **Function output schema** — optional `output` on functions declares the return type schema. Accepts explicit schema, `$ref`, or `infer` (default). Inference derives schema from terminal blocks.
  - **CEL as universal expression language** — `{{...}}` template expressions evaluate CEL, not just dotted paths. Unifies guards, `set_context`, `set_workflow`, and templates under one expression language.
  - **Two-level declared context** — workflow context (`workflow.*`, cross-function) and function context (`context.*`, per-invocation). Variables declared with type and initial value. Blocks can only write pre-declared variables. No `has()` boilerplate.
  - **Conversation-transparent call blocks** — callee starts empty conversation, caller's history unchanged (clarified from misleading "reset" language).
  - **Agent configuration block** — mech targets reel (agent runtime) not flick (raw LLM). `model` moved inside `agent` block alongside `grant` (ToolGrant flags), `tools` (custom tool names), `write_paths`, and `timeout`. Three-level cascade (workflow → function → block) with replace semantics. Named agent configs (`agents` map, parallel to `schemas`) with `$ref:#name` and `extends` for inheritance with overrides.

**Next Work:**

Mech implementation is broken into 17 incremental TDD deliverables — see `docs/MECH_SPEC.md` §13 for full details (scope, tests-first list, acceptance criteria per deliverable). Per-deliverable cycle: write tests → implement → `cargo test`/`clippy`/`fmt` → `/review` → update STATUS.

Deliverables (strictly sequential except 9↔10 which can overlap):

1. ~~Crate skeleton & error types~~ ✅
2. ~~YAML schema types (parse-only, serde)~~ ✅
3. ~~CEL expression compilation & evaluation (5 namespaces, template interpolation)~~ ✅
4. ~~Schema registry & JSON Schema handling (`$ref` resolution)~~ ✅
5. ~~Load-time validation (the 24+ checks from §10)~~ ✅
6. ~~Schema inference for function outputs (`output: infer`)~~ ✅
7. ~~Workflow loader (end-to-end load pipeline)~~ ✅
8. ~~Context & state management (workflow/context/block namespaces)~~ ✅
9. ~~Prompt block executor (agent cascade, structured output)~~ ✅
10. ~~Call block executor (three input forms, output mapping)~~ ✅
11. ~~Transitions & block scheduling (imperative mode, guards, self-loops)~~ ✅
12. Function executor & workflow runtime (imperative + dataflow modes)
13. Conversation management & history scoping
14. Cue integration (`MechTask` implementing `cue::TaskNode`)
15. CLI (`mech run`, `mech validate`)
16. End-to-end integration test suite (hermetic, fake LLM)
17. Documentation polish & examples

**Immediate next action:** Deliverable 12 — Function executor & workflow runtime (imperative + dataflow modes).

---

## Epic

**Phase:** Core orchestration, knowledge layer, file-level review, and cue integration complete. 251 tests passing. All orchestrator tests exercise `cue::Orchestrator<EpicStore<A>, EventLog>`.

**Implemented:**
- Recursive problem-solver with DFS execution, retry/escalation, fix loops, recovery re-decomposition
- `EpicTask<A>` implementing `cue::TaskNode` (bridges Task + AgentService to cue's trait contract)
- `EpicStore<A>` implementing `cue::TaskStore` (wraps EpicState + runtime deps)
- `TaskRuntime<A>` (agent, vault, events, limits, project_root) injected into tasks at construction
- `ReelAgent` adapter (14 AgentService methods)
- State persistence (`.epic/state.json`), resume, cycle-safe DFS
- TUI (ratatui + crossterm), CLI (init, run, resume, status, setup)
- Process sandboxing via reel/lot, AppContainer prerequisites check
- Context propagation (TaskContext, structural map, discovery flow, checkpoints)
- Assessment (Haiku), verification & fix loops (model escalation Haiku->Sonnet->Opus)
- Recovery (Opus assessment, incremental/full re-decomposition, budget inheritance)
- Usage tracking (per-task accumulation, TUI metrics, headless summary)
- File-level review (leaf tasks, post-verification semantic review)
- Vault integration (document store, ResearchQuery tool, discovery recording, reorganize)
- Research Service gap-filling (vault query -> gap identification -> codebase/web exploration -> synthesis)
- Branch verification separation (three sequential agent calls: correctness, completeness, aggregate simplification — fail-fast)
- Simplification review (local leaf simplification after file-level review, aggregate branch simplification in branch verification)

**Not Implemented:**
- User-level config fallback (`~/.config/epic/config.toml`)

**Next Work:**
1. **User-level config fallback** — `~/.config/epic/config.toml` resolution for user defaults.
