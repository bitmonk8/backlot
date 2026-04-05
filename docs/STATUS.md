# Backlot — Status

## Flick

**Phase:** Complete. 373 tests passing, zero clippy errors.

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

**Next Work:**
- reqwest 0.13 upgrade (intermittent rustc crashes on this machine are CPU-related — Intel i9-14900K instability — not a windows-sys or rustc bug)

---

## Lot

**Phase:** Implementation and audit remediation complete. CI green on Linux, macOS, Windows.

**Implemented:**
- Cross-platform process sandboxing (Linux namespaces+seccomp, macOS Seatbelt, Windows AppContainer)
- Policy builder API, path grants, stdio management, timeout handling
- Full test suite running in parallel

**Issues:** 69 NIT-level findings, 0 MUST FIX, 0 NON-CRITICAL. See [ISSUES.md](ISSUES.md).

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

## Epic

**Phase:** Core orchestration, knowledge layer, and file-level review implemented. 246 tests passing.

**Implemented:**
- Recursive problem-solver with DFS execution, retry/escalation, fix loops, recovery re-decomposition
- `ReelAgent` adapter (10 AgentService methods)
- State persistence (`.epic/state.json`), resume, cycle-safe DFS
- TUI (ratatui + crossterm), CLI (init, run, resume, status, setup)
- Process sandboxing via reel/lot, AppContainer prerequisites check
- Context propagation (TaskContext, structural map, discovery flow, checkpoints)
- Assessment (Haiku), verification & fix loops (model escalation Haiku->Sonnet->Opus)
- Recovery (Opus assessment, incremental/full re-decomposition, budget inheritance)
- Usage tracking (per-task accumulation, TUI metrics, headless summary)
- File-level review (leaf tasks, post-verification semantic review)
- Vault integration (document store, ResearchQuery tool, discovery recording, reorganize)
- Research Service gap-filling (vault query -> gap identification -> codebase exploration -> synthesis)
- Branch logic extraction (`task/branch.rs`), leaf extraction (`task/leaf.rs`)
- 28 files, 12,740 lines (6,373 core, 6,367 test)

**Not Implemented:**
- Simplification review (local leaf + aggregate branch)
- Branch verification separation (single call, not split into correctness + completeness + simplification)
- User-level config fallback (`~/.config/epic/config.toml`)

**Next Work:**
1. **Orchestrator extraction** — Extract orchestrator, task, state, events, agent trait, and config types into a standalone sibling crate. See [epic/ORCHESTRATOR_EXTRACTION.md](epic/ORCHESTRATOR_EXTRACTION.md).
2. **Branch verification separation** — Split single-call branch verification into correctness + completeness + aggregate simplification reviews.
3. **Web search scope for Research Service** — Add WEB scope to gap-filling pipeline (vault + codebase + web search).
4. **User-level config fallback** — `~/.config/epic/config.toml` resolution for user defaults.
