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

**Issues:** 15 open (tracked in GitHub Issues).

**Next Work:** None identified.

---

## Lot

**Phase:** Implementation and audit remediation complete. 278 tests passing, CI green on Linux, macOS, Windows.

**Implemented:**
- Cross-platform process sandboxing (Linux namespaces+seccomp, macOS Seatbelt, Windows AppContainer)
- Policy builder API, path grants, stdio management, timeout handling
- Full test suite running in parallel

**Issues:** 55 open (NIT-level, tracked in GitHub Issues).

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

**Issues:** 22 open (tracked in GitHub Issues).

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

**Issues:** 55 open (tracked in GitHub Issues).

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

**Phase:** Complete. 326 tests passing (300 unit + 26 integration), zero clippy warnings.

**Spec** (`docs/MECH_SPEC.md`):
- Standalone crate providing a declarative YAML-based workflow definition format (not a custom-grammar language). Depends on cue (TaskNode integration) and reel (agent execution).
- All 12 sections drafted and reviewed. Covers: motivation, design goals, unified CDFG model, conversation model (history scoping, compaction, agent configuration), block specification (prompt + call blocks, field validity), transitions & guards (CEL, ordered evaluation, self-loops), template expressions & scoping (5 namespaces, CEL everywhere), schema handling (inline + `$ref` + workflow-level shared schemas), context & state (two-level declared variables, `set_context`/`set_workflow`), validation & error handling (24+ load-time checks, 5 runtime error types), cue integration (function = leaf task, model escalation interaction), YAML reference schema with complete worked example.

**Issues:** 52 open (tracked in GitHub Issues).

**Next Work:** None identified.

**Recent:** Schema subsystem consolidation — 10 issues closed (#3, #14, #15, #19, #20, #28, #32, #36, #46, #288). `SchemaRegistry` now stores resolved JSON bodies; prompt executor uses pre-compiled validators; `$ref:#name` parsing consolidated into `parse_named_ref`/`try_parse_named_ref`; `ResolvedSchema::validate` replaces `SchemaRegistry::validate`; `SchemaInferDeferred` error variant; `BlockDef::transitions()`/`depends_on()` accessors; `schema/mod.rs` split into submodules.

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

**Issues:** 74 open (tracked in GitHub Issues).

**Next Work:**
1. **User-level config fallback** — `~/.config/epic/config.toml` resolution for user defaults.
