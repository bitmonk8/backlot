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

**Phase:** Complete. 331 tests passing (305 unit + 26 integration), zero clippy warnings.

**Spec** (`docs/MECH_SPEC.md`):
- Standalone crate providing a declarative YAML-based workflow definition format (not a custom-grammar language). Depends on cue (TaskNode integration) and reel (agent execution).
- All 12 sections drafted and reviewed. Covers: motivation, design goals, unified CDFG model, conversation model (history scoping, compaction, agent configuration), block specification (prompt + call blocks, field validity), transitions & guards (CEL, ordered evaluation, self-loops), template expressions & scoping (5 namespaces, CEL everywhere), schema handling (inline + `$ref` + workflow-level shared schemas), context & state (two-level declared variables, `set_context`/`set_workflow`), validation & error handling (24+ load-time checks, 5 runtime error types), cue integration (function = leaf task, model escalation interaction), YAML reference schema with complete worked example.

**Issues:** 37 open (tracked in GitHub Issues).

**Next Work:** None identified.

**Recent:** validate.rs restructuring — 12 issues closed (#1, #4, #16, #18, #22, #48, #50, #51, #52, #55, #56, #287). Split `validate.rs` (3766 lines) into `validate/` directory with 7 submodules (mod.rs, model.rs, report.rs, blocks.rs, agents.rs, cel_check.rs, graph.rs, helpers.rs). CEL reference-extraction helpers moved to `cel.rs`. `BlockDef::set_context()`/`set_workflow()` accessors added. `check_type` in context.rs replaced with JSON Schema validation. Prompt/Call arm duplication resolved via shared `BlockDef` accessors. Uniform/PerCall call blocks now handled in `collect_block_fields`/`collect_block_required_fields`. Dominator computation simplified. Agent ref naming clarified (`validate_agent_ref_strict` vs `validate_agent_ref`). `check_*` methods renamed to `validate_*`. `CollectedRefs.block_refs` Option wrapper removed. Dataflow cycle message direction fixed. Missing extends target deduplication. Unused `_fn_name` parameter removed. `CelCheckCtx` struct replaces too-many-arguments suppressions.

Schema subsystem consolidation — 10 issues closed (#3, #14, #15, #19, #20, #28, #32, #36, #46, #288). `SchemaRegistry` now stores resolved JSON bodies; prompt executor uses pre-compiled validators; `$ref:#name` parsing consolidated into `parse_named_ref`/`try_parse_named_ref`; `ResolvedSchema::validate` replaces `SchemaRegistry::validate`; `SchemaInferDeferred` error variant; `BlockDef::transitions()`/`depends_on()` accessors; `schema/mod.rs` split into submodules.

MechError variant naming cleanup — 7 issues closed (#35, #37, #39, #47, #54, #59, #286). `SchemaValidationFailure` Display no longer includes raw LLM output. `Validation` renamed to `WorkflowValidation`. `InferenceFailed` renamed to `OutputSchemaInferenceFailed`. `SchemaInvalid` split into `SchemaInvalid` (named shared schemas) and `InlineSchemaInvalid` (inline schemas). `CelEvaluation` for namespace binding replaced with dedicated `CelNamespaceBind` variant. `YamlParse.path` changed from `PathBuf` to `Option<PathBuf>`. CLI-local `CliError` enum introduced (`InputParse`, `OutputSerialize`) replacing `MechError::Io` misuse in mech-cli.

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
