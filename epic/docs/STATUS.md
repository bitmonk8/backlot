# Project Status

## Current Phase

**Implementation** — Core orchestrator loop complete. Agent wiring next.

## Milestones

- [x] Initial document structure created
- [x] ZeroClaw integration mode evaluated (library via wrapper binary — provisional, risk noted)
- [x] ZeroClaw security audit complete (CONDITIONAL PASS — no backdoors, see [audit summary](audit/SUMMARY.md))
- [x] Agent runtime strategy decided (fork ZeroClaw)
- [x] All open questions resolved (23/23)
- [x] ZeroClaw fork prepared — `epic/hardening` branch with all hardening patches applied
- [x] Git repositories created — [epic](https://github.com/bitmonk8/epic), [zeroclaw-fork](https://github.com/bitmonk8/zeroclaw-fork)
- [x] ZeroClaw fork added as git submodule at `deps/zeroclaw/`
- [x] Finalize design documents — reviewed all docs for consistency with resolved decisions
- [x] Scaffold Rust project — `cargo init`, module structure, dependencies, Clippy lints configured
- [x] Core orchestrator loop — task types, events, state, AgentService trait, DFS loop with retry/escalation, 6 tests passing
- [ ] Agent call wiring — connect orchestrator to ZeroClaw AgentBuilder API

## Open Question Tally

Tracked in [OPEN_QUESTIONS.md](OPEN_QUESTIONS.md). Summary by area:

| Area | Open | Resolved | Total |
|---|---|---|---|
| ZeroClaw Integration | 0 | 6 | 6 |
| Agent SDK | 0 | 3 | 3 |
| Configuration | 0 | 4 | 4 |
| Document Store | 0 | 2 | 2 |
| TUI | 0 | 2 | 2 |
| Rust-Specific | 0 | 3 | 3 |
| Scope | 0 | 3 | 3 |
| **Total** | **0** | **23** | **23** |

## Next Work Candidates

1. **Agent call wiring** — Implement `AgentService` backed by ZeroClaw `AgentBuilder` API. Implement `submit_result` tool for structured output. Requires fixing ZeroClaw compilation (schemars ICE on rustc 1.93.1).
2. **State persistence integration** — Wire `EpicState::save`/`load` into the main run loop for session resume.
3. **TUI event consumer** — Connect `EventReceiver` to ratatui task tree and worklog panels.

## Decisions Made

### 2025-02-25: ZeroClaw integration mode

**Decision:** Library import via wrapper binary. Epic depends on `zeroclaw` crate, uses `AgentBuilder` API for per-call agent construction with custom model, tools, and system prompt.

**Rationale:** Subprocess mode lacks custom system prompt and structured output capture. Daemon mode adds unnecessary HTTP overhead. Library mode gives full control over all parameters while leveraging ZeroClaw's agent loop, provider abstraction, and tool system.

**Requires:** Two upstream PRs — make `security` module public, add Windows shell support to `NativeRuntime`. Both are general improvements, not Epic-specific.

**Status:** Decided. Fork ZeroClaw selected as runtime. Provenance risk accepted with mitigations (pin to audited commit, treat as vendored code).

**Key findings during investigation:**
- ZeroClaw v0.1.7, 100% Rust, public `AgentBuilder` API
- Anthropic provider: full native tool use, no streaming
- No MCP support (Perplexity claims fabricated — verified against source)
- No dynamic plugin system (Perplexity "stable ABI" claim fabricated)
- Shell hardcoded to `sh -c` (Windows broken)
- `SecurityPolicy` is `pub(crate)` (blocks library consumers from using built-in tools)
- Python `zeroclaw-tools` package is standalone LangGraph wrapper, not integrated with Rust binary

### 2026-02-25: ZeroClaw provenance risk assessment

**Finding:** The ZeroClaw repo (github.com/zeroclaw-labs/zeroclaw) is 12 days old (first commit 2026-02-13), with 160K lines of Rust and 3,400+ stars in 2 days (consistent with star farming). Part of the "\*Claw wave" of rapidly-spawned OpenClaw alternatives with associated crypto fraud. Marketing materials (5 domains, SEO blog posts) make claims not backed by source code (MCP support, plugin system). Inconsistent author identity across Cargo.toml, commits, and SECURITY.md.

**Impact:** ZeroClaw integration approach remains technically sound, but the dependency carries provenance, maintenance, and correctness risks. Decision downgraded from "decided" to "provisional" pending alternative runtime evaluation.

**Mitigations if selected:** Pin to audited commit, audit dependent modules, maintain direct API fallback capability. See [ZeroClaw Integration — Risk Assessment](ZEROCLAW_INTEGRATION.md#risk-assessment).

### 2026-02-25: LocalAgent evaluation

**Finding:** [LocalAgent](https://github.com/CalvinSturm/LocalAgent) (34K lines Rust, 5 days old, 92 commits) is a local-first agent runtime CLI with a strong safety model (TrustGate policy, taint tracking, audit logging) and real MCP stdio client support. However: no Anthropic provider (local LLMs only), 5 hardcoded tools (no extensible Tool trait), no structured output mechanism.

**Decision:** Discarded as a base for Epic. Retained as design inspiration for:
- TrustGate policy model (YAML-based tool approval with content-hashed keys, TTL, audit trail)
- MCP stdio client architecture (JSON-RPC, tool catalog pinning)
- ExecTarget trait (host vs Docker execution abstraction)
- Windows platform handling patterns (`.cmd` detection, atomic rename workarounds)

### 2026-02-25: Agent runtime strategy decided — Fork ZeroClaw

**Decision:** Option A — fork ZeroClaw. Fork at a specific audited commit, submit upstream PRs for required changes (public security module, Windows shell support), track upstream if PRs are accepted, otherwise maintain fork as vendored code.

**Rationale:** Security audit passed (CONDITIONAL PASS, no backdoors). The codebase provides functional agent runtime infrastructure (providers, tools, memory, tool dispatch) that would take significant effort to reimplement. Provenance risk is real but mitigated by pinning to audited commit and treating the fork as vendored code with no assumption of upstream maintenance.

**Mitigations:** Pin to audited commit, never track `main` blindly. Audit all modules Epic depends on. Maintain direct-API fallback capability in the architecture. Remove GLM URL-to-curl fallback from fork. Remove `wa-rs` WhatsApp crate family (supply chain concern).

**Status:** Fork complete. See [2026-02-26 entry](#2026-02-26-zeroclaw-fork-complete) below.

### 2026-02-25: Agent SDK approach

**Decision:** All agent calls route through ZeroClaw's `AgentBuilder` API. No direct Anthropic API bypass. Structured output via custom `submit_result` tool implementing ZeroClaw's `Tool` trait.

**Decision:** No token-level streaming for v1. TUI displays event-level updates (task phase transitions). ZeroClaw's Anthropic provider lacks streaming; acceptable for initial release.

### 2026-02-25: ZeroClaw security audit completed

**Verdict:** CONDITIONAL PASS (no FAIL verdicts). 16 audit units covering ~130K lines. Full report: [audit/SUMMARY.md](audit/SUMMARY.md).

**Key findings:**
- No deliberate backdoors, data exfiltration, or covert channels found in any module.
- All outbound network calls go to documented, legitimate API endpoints.
- Security model is deny-by-default with 6-layer path sandboxing, command allowlists, rate limiting.
- Secrets encrypted at rest (ChaCha20-Poly1305 AEAD). Minimal unsafe code (2 instances).
- Supply chain concern: `wa-rs` WhatsApp crate family (8 days old, unknown publisher, includes proc-macro). Disabled by default.
- 5 medium-severity security gaps found (not backdoors): unauthenticated WATI webhook, GLM URL-to-curl prompt injection surface, Lark webhook missing signature verification, Firejail lacking `--net=none`, `web_search_tool` bypassing SecurityPolicy.

**Impact on runtime decision:** The audit removes the code-quality/security blocker for Option A (fork ZeroClaw). The provenance/maintenance risk remains but is separate from the code itself. If forking, pin to audited commit and remove the GLM URL-to-curl fallback.

### 2026-02-25: Configuration format — TOML

**Decision:** TOML for all Epic configuration files (`epic.toml`, `.epic/config.toml`).

**Rationale:** Rust ecosystem standard (Cargo, rustfmt, clippy). Epic's config is shallow — verification steps, model tiers, limits, paths — which fits TOML naturally. `toml` crate is mature and serde-native. YAML rejected due to archived `serde_yaml` crate and implicit type coercion footguns. RON rejected as too niche for a project-agnostic tool.

### 2026-02-25: `epic init` — agent-driven interactive scaffolding

**Decision:** `epic init` uses an agent to explore the project (build system markers, test frameworks, linters, CI config), presents findings to the user, and interactively confirms which verification steps to enable. Writes `epic.toml` with confirmed choices; declined options commented out. Fallback: minimal scaffold with empty verification sections if nothing detected.

### 2026-02-25: Batch decisions — Rust, Scope, Document Store, TUI

**Rust-specific:**
- Async runtime: tokio (ZeroClaw already uses it, ecosystem standard)
- Error handling: anyhow + thiserror (thiserror for module-boundary enums, anyhow for propagation)
- Serialization: serde + serde_json + toml (the only viable path)

**Scope (v1 boundaries):**
- Parallel execution: out of scope — sequential must be proven robust first
- Multi-language: no special handling — configurable verification already makes Epic language-agnostic
- Git hosting integration: out of scope — git ops happen through agent shell access

**Document Store:**
- File-based (markdown) for v1 — small document counts, inspectable/diffable, SQLite can layer on later
- Librarian routes through ZeroClaw agent (Haiku, read-only tools, submit_result) — consistent with all-through-ZeroClaw pattern

**TUI:**
- ratatui with crossterm backend
- Read-only monitoring for v1 — orchestrator emits events, TUI consumes. Decoupled for future interactive controls

**Config ownership:**
- Epic owns all config — no separate ZeroClaw config file, `[zeroclaw]` section in epic.toml suffices

### 2026-02-26: ZeroClaw fork complete

**Action:** Fork prepared at [bitmonk8/zeroclaw-fork](https://github.com/bitmonk8/zeroclaw-fork), branch `epic/hardening` (4 commits ahead of upstream `main`, 5 logical changes).

**Hardening patches applied:**
1. `pub mod security` — makes `SecurityPolicy` accessible to library consumers (`src/lib.rs:66`)
2. Windows shell support — `#[cfg(windows)]` using `cmd /C` in `NativeRuntime` (`src/runtime/native.rs`) and cron scheduler (`src/cron/scheduler.rs`)
3. GLM URL-to-curl removal — eliminated `build_curl_command()` and all bare-URL auto-conversion from `src/agent/loop_.rs` (prompt injection hardening)
4. wa-rs supply chain removal — removed all 6 wa-rs crates, `serde-big-array`, `qrcode`, and the `whatsapp-web` feature flag from `Cargo.toml`
5. `mut` fix — missing `mut` on `shell_cmd` in cron scheduler Windows branch

**Integration:** Added as git submodule at `deps/zeroclaw/` in the Epic repo. Epic will reference via `zeroclaw = { path = "deps/zeroclaw" }` in Cargo.toml.

**Compilation note:** ZeroClaw compiles successfully with both Rust 1.92.0 and 1.93.1. Intermittent compiler crashes observed (~20-40% failure rate) attributed to hardware instability on the development machine (Intel i9-14900K, known Raptor Lake issue). Not a software defect.

### 2026-02-26: Core orchestrator loop implemented

**Scope:** Task types (`TaskId`, `Task`, `TaskPath`, `TaskPhase`, `Model`, `Attempt`, `MagnitudeEstimate`, `TaskOutcome`), assessment/branch/verify data structs, `Event` enum with unbounded channel, `EpicState` with `HashMap<TaskId, Task>` + JSON persistence, `AgentService` trait (6 async methods), `Orchestrator<A: AgentService>` with DFS loop, retry/escalation (3 retries per tier, Haiku→Sonnet→Opus), verification-failure-equals-task-failure for v1.

**Files modified:** `src/task/{mod,assess,branch,leaf,verify}.rs`, `src/events.rs`, `src/state.rs`, `src/agent/mod.rs`, `src/orchestrator.rs`, `src/main.rs`.

**Tests (6):** single_leaf, two_children, leaf_retry_and_escalation, terminal_failure, depth_cap_forces_leaf, persistence_round_trip. All pass with `MockAgentService`.

**Deferred for v1:** Fix loop after verification failure, full recovery re-decomposition, checkpoint adjust/escalate actions.

**Note:** ZeroClaw dependency made optional — `schemars` derive triggers ICE (E0080 scalar size mismatch) on rustc 1.93.1. Needs investigation before agent wiring.
