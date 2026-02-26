# ZeroClaw Integration

## Status

**Complete.** ZeroClaw forked, hardening patches applied, added as submodule at `deps/zeroclaw/`. See [Fork Status](#fork-status) and [Risk Assessment](#risk-assessment).

## Role

ZeroClaw serves as the **agent runtime** — the execution environment for AI agent sessions with tool execution, provider abstraction, and conversation management. Epic is the **orchestrator** — it decides what work to do, which model to use, which tools to grant, and what system prompt to send. ZeroClaw executes individual agent calls; Epic manages the recursive task tree.

## Integration Mode: Wrapper Binary

Epic depends on the `zeroclaw` crate as a Rust library and uses the `AgentBuilder` API to construct agents per-call.

```
Epic Orchestrator (owns the task loop)
  │
  │  For each agent call:
  │
  ├─ 1. Select model (Haiku/Sonnet/Opus per assessment)
  ├─ 2. Build tool set (per-phase: READ, WRITE, BASH, etc.)
  ├─ 3. Assemble system prompt (role + context + instructions)
  ├─ 4. Construct Agent via AgentBuilder
  ├─ 5. Call agent.run_single(prompt)
  ├─ 6. Read response + agent.history() for structured output
  └─ 7. Drop agent (stateless per-call)
```

### Why This Mode

| Alternative | Why not |
|---|---|
| Subprocess (`zeroclaw agent -m`) | No custom system prompt flag. stdout is plain text. No structured output capture. |
| Daemon (gateway API) | Overkill for one-shot calls. Adds HTTP serialization overhead. |
| Library (this approach) | Full control over model, tools, prompt, output. In-process, lowest latency. |

### Minimal Wiring

```rust
use zeroclaw::agent::{Agent, AgentBuilder};
use zeroclaw::agent::dispatcher::NativeToolDispatcher;
use zeroclaw::agent::prompt::SystemPromptBuilder;
use zeroclaw::memory::NoneMemory;
use zeroclaw::observability::NoopObserver;
use zeroclaw::providers;

let provider = providers::create_provider("anthropic", Some(&api_key))?;
let tools = epic_build_tool_set(phase, &runtime, &security); // Epic's per-phase tool selection

let mut agent = AgentBuilder::new()
    .provider(provider)
    .tools(tools)
    .memory(Arc::new(NoneMemory))
    .observer(Arc::new(NoopObserver))
    .tool_dispatcher(Box::new(NativeToolDispatcher))
    .model_name(model.into())        // "claude-haiku-4-5-20251001", etc.
    .temperature(0.3)
    .workspace_dir(project_root)
    .build()?;

let response = agent.run_single(&prompt).await?;
let history = agent.history(); // inspect tool calls, structured output
```

## Upstream PRs (Applied in Fork)

Both changes are applied in the `epic/hardening` branch and have dedicated PR branches for upstream submission:

### 1. Make `security` module public (branch: `pr/public-security`)

**File:** `src/lib.rs:66` — `pub(crate) mod security` → `pub mod security`

**Why:** `SecurityPolicy` is required by every built-in tool. Without this, library consumers cannot construct any built-in tools.

### 2. Windows shell support in NativeRuntime (branch: `pr/windows-shell`)

**Files:** `src/runtime/native.rs:42`, `src/cron/scheduler.rs:430` — `#[cfg(windows)]` branch using `cmd /C`.

**Why:** ZeroClaw ships Windows binaries but shell commands were hardcoded to `sh -c`.

## What ZeroClaw Provides

- **Agent execution loop** — multi-turn tool-calling loop with configurable max iterations
- **Anthropic provider** — native tool use format, API key + OAuth auth, all Claude model IDs
- **Tool system** — public `Tool` trait, built-in tools (shell, file read/write/edit, glob, grep, git, HTTP, browser, memory)
- **Security policy** — command allowlists, workspace scoping, autonomy levels
- **Conversation history** — full message history including tool calls and results
- **Provider abstraction** — swap Anthropic/OpenAI/Ollama/OpenRouter via config

## What ZeroClaw Does NOT Provide

- **No MCP support** — verified against source. Perplexity claims fabricated.
- **No Anthropic streaming** — provider implements `chat` but not `stream_chat`
- **No dynamic plugin loading** — no dylib/ABI system. Perplexity "stable ABI" claim fabricated.
- **No per-session tool scoping via config** — tools are set at agent construction time (which is fine for Epic's approach)
- **No custom system prompt via CLI** — only via `AgentBuilder::prompt_builder()` (library mode)

## Capability Mapping

Epic's per-phase tool grants map to ZeroClaw tool construction:

| Epic Tools Flag | ZeroClaw Tools |
|---|---|
| NONE | `[submit_result]` only |
| READ | `[FileReadTool, GlobTool, ContentSearchTool, submit_result]` |
| WRITE | READ + `[FileWriteTool, FileEditTool]` |
| BASH | `[ShellTool]` (with custom WindowsRuntime on Windows) |
| WEB | `[HttpRequestTool, WebFetchTool, WebSearchTool]` |
| ALL | READ + WRITE + BASH + WEB + `[submit_result]` |

## Structured Output: submit_result Tool

Custom `Tool` implementation compiled into Epic:

```rust
struct SubmitResultTool {
    captured: Arc<Mutex<Option<serde_json::Value>>>,
}

impl Tool for SubmitResultTool {
    fn name(&self) -> &str { "submit_result" }
    fn description(&self) -> &str { "Submit structured result for this task" }
    fn parameters_schema(&self) -> Value {
        // JSON Schema matching the expected output type
        // (varies per agent call — assessment, verification, etc.)
    }
    async fn execute(&self, args: Value) -> Result<ToolResult> {
        *self.captured.lock().unwrap() = Some(args);
        Ok(ToolResult { success: true, output: "Result submitted.".into(), error: None })
    }
}
```

Epic constructs `SubmitResultTool` with a shared `Arc<Mutex>`, passes it in the tool set, runs the agent, and reads the captured value after execution.

## Dependency Considerations

ZeroClaw pulls ~771 packages (full lockfile). Non-optional deps include reqwest, axum, rusqlite (bundled), nostr-sdk, ring, image, lettre, etc. This is heavy. Mitigations:

- Use `default-features = false` to avoid optional features
- Accept the dependency cost for v1; evaluate slimming later
- Pin to specific git revision for reproducibility

## Future Evolution

ZeroClaw provides a foundation beyond v1:
- Memory system could back DocumentStore queries
- Channel system could enable Slack/Discord integration for Epic notifications
- Provider routing (hint-based) could support multi-provider strategies
- Docker runtime mode could provide stronger agent isolation

## Risk Assessment

Investigation of the ZeroClaw repository provenance (conducted 2026-02-25) raised concerns.

### Facts

| Metric | Value |
|---|---|
| First commit | 2026-02-13 |
| Investigation date | 2026-02-25 (12 days of history) |
| Total commits | 2,243 in 11 days (~200/day) |
| Lines of Rust | 160,785 |
| Unique committer emails | 130+ |
| GitHub stars | 3,400+ in first 2 days |
| Registered domains | 5 (zeroclaw.org, .net, .bot, .dev, .app) |

### Concerns

**Project maturity.** 12 days old, v0.1.7. The codebase is large (160K lines of Rust) and functional, but the project has no track record of maintenance, breaking change discipline, or security response. API stability is unknown.

**Star farming pattern.** 3,400+ stars in 2 days is consistent with bot-driven star inflation. The parent project OpenClaw had documented bot inflation (500K fake Moltbook users from a single agent).

**SEO astroturfing.** Five domains with polished content for a 12-day-old project. Blog posts and marketing copy make claims not backed by source code (MCP support, stable ABI plugin system, dynamic tool loading). Perplexity consistently hallucinated features — likely because it ingested this marketing material as fact.

**Inconsistent provenance.** Cargo.toml author: "theonlyhennygod". Most commits: "Chummy"/"Chum Yin". SECURITY.md references `github.com/theonlyhennygod/zeroclaw` (personal repo), not `github.com/zeroclaw-labs/zeroclaw` (org repo we cloned).

**Ecosystem context.** Part of the "\*Claw wave" — OpenClaw spawned ZeroClaw, PicoClaw, IronClaw, NullClaw, NanoBot, TinyClaw within days. Crypto scammers launched fake tokens exploiting these names. Issue [#527](https://github.com/zeroclaw-labs/zeroclaw/issues/527) warns about fraud and impersonation.

**Fraud prevention issue.** The project's own issue #527 (2026-02-17) warns of "bad actors impersonating ZeroClaw team members" and states the official website and Discord "are not ready yet" — despite 5 domains already populated with content.

### Risk Summary

| Risk | Severity | Impact on Epic |
|---|---|---|
| Project disappears or abandons maintenance | Medium | Epic loses upstream; must self-maintain or replace |
| Young codebase has undiscovered correctness bugs | Medium | Agent execution errors, security vulnerabilities |
| Upstream introduces breaking API changes | Medium | Fork diverges, maintenance burden increases |
| Reputational association with fraud ecosystem | Low | Perception risk if Epic credits ZeroClaw publicly |
| Supply chain compromise (malicious code injected) | Low-Medium | Must audit code before depending on it |

### Mitigation

If ZeroClaw is selected despite these risks:
- Pin to a specific audited git commit, never track `main`
- Audit the specific modules Epic depends on (agent, providers/anthropic, tools, runtime)
- Maintain ability to swap to direct API integration (Option 3 fallback)
- Do not depend on upstream maintenance — treat as vendored code

### Fallback: Direct API Integration

If the alternative runtime evaluation does not identify a better option, and ZeroClaw risk is deemed too high, Epic can build its own thin agent layer:
- Anthropic API calls via `reqwest` (~200 lines)
- Tool registry with per-call scoping (~150 lines)
- Tool execution loop (~200 lines)
- Structured output via tool-call capture (~100 lines)
- Total: ~500-800 lines, zero external runtime dependency, full control

## Alternative Evaluated: LocalAgent

[LocalAgent](https://github.com/CalvinSturm/LocalAgent) (34K lines Rust, 5 days old) was evaluated and **discarded as a base** — no Anthropic provider, no extensible Tool trait, no structured output. However, several design ideas are worth borrowing:

| Idea | How it applies to Epic |
|---|---|
| **TrustGate policy model** | YAML-based tool approval with content-hashed keys, TTL, audit trail. More principled than simple allowlists. |
| **MCP stdio client** | JSON-RPC client with tool catalog pinning. Future path for external tool integration. |
| **ExecTarget trait** | Clean host vs Docker abstraction. Future sandboxing path. |
| **Windows platform handling** | `.cmd` extension detection for subprocess spawning, atomic rename workarounds. |

## Fork Status

**Complete.** Fork at [bitmonk8/zeroclaw-fork](https://github.com/bitmonk8/zeroclaw-fork), branch `epic/hardening`. Added as git submodule at `deps/zeroclaw/` in the Epic repo.

### Patches applied (5 commits)

| Commit | Change | Files |
|---|---|---|
| `2be0012` | `pub mod security` — library consumers can access `SecurityPolicy` | `src/lib.rs` |
| `1b5cfa3` | Windows shell support — `#[cfg(windows)]` using `cmd /C` | `src/runtime/native.rs`, `src/cron/scheduler.rs` |
| `4e16f8b` | Remove URL-to-curl auto-conversion (prompt injection hardening) | `src/agent/loop_.rs` |
| `5dbb203` | Remove wa-rs/qrcode supply chain deps, fix `mut shell_cmd` | `Cargo.toml`, `Cargo.lock`, `src/cron/scheduler.rs` |

### Upstream PR strategy

PR-ready branches exist (`pr/public-security`, `pr/windows-shell`) for submitting to upstream. If accepted, fork maintenance burden decreases. If rejected or upstream goes inactive, the fork is self-contained as vendored code.

### Usage

```toml
# Epic's Cargo.toml
[dependencies]
zeroclaw = { path = "deps/zeroclaw" }
```
