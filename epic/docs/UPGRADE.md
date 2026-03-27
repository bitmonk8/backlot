# Epic Upgrade: Reel Dependency and Observability Fields

## Context

Reel has added three capabilities since epic's current pinned revision (`51eb559`):

1. **Session transcript** — `Vec<TurnRecord>` on `RunResult`, capturing every tool call, per-turn token usage, and API latency across an agent session.
2. **Cache token fields** — `cache_creation_input_tokens` and `cache_read_input_tokens` in the `Usage` struct. These reflect Anthropic prompt caching enabled by flick's new 2-breakpoint strategy.
3. **Per-call API latency** — `api_latency_ms` available both as a session-level field and per-turn in the transcript.

Epic's reel dependency must be updated to pick up these changes.

## What Needs to Change

### 1. Update reel dependency

Bump reel from rev `51eb559` to at least `93f35ef` (or later). This is the revision that includes transcript, cache token fields, and timing.

Note: `RunResult` now has a `transcript` field. If epic deserializes or pattern-matches on `RunResult` fields, this may require code adjustments.

### 2. Capture RunResult metadata from agent sessions

The `ReelAgent` adapter currently extracts only `.output` from `RunResult` and discards `.usage`, `.tool_calls`, `.transcript`, and `.response_hash`. After the reel upgrade, `run_request()` should return these alongside the structured output so the orchestrator can use them.

### 3. Aggregate usage per task

Each `Task` in epic's state goes through multiple agent phases (assess, decompose, execute, verify, checkpoint). Each phase produces a `RunResult` with usage data. Epic should accumulate per-task totals so the state file and TUI can report:

- Total tokens (input + output) per task
- Cache hit ratio (cache_read vs total input tokens)
- API latency per phase
- Cost per task and total run cost

### 4. Persist usage in state.json

`Task` and `EpicState` should include accumulated usage so that:
- Resume can report cost-so-far.
- Post-run analysis can identify expensive tasks or phases.
- Budget limits (if implemented) can reference actual spend.

### 5. Surface usage in TUI and CLI output

- **TUI metrics panel**: Currently shows task counts by phase. Should also show token usage per model tier and total cost.
- **Headless output** (`--no-tui`): Final summary should include total usage/cost.
- **Status command**: Should show accumulated usage from state.json.

### 6. Expose session transcript (optional but recommended)

Per-task transcripts would enable post-run debugging of agent behavior — what tools were called, what the model decided at each step, whether retries or escalations were warranted. Options:

- Write transcripts to `.epic/transcripts/<task_id>.json` alongside state.json.
- Include in the event stream (new event type) so the TUI worklog can display tool-call detail.

## What Does NOT Need to Change

- **Prompt caching** works automatically after the reel update. Flick handles `cache_control` injection. Multi-turn sessions in epic's leaf execution will benefit from cached system prompts and tool definitions with no code changes.
- **Structured output validation** (fence stripping, required-field checks) is handled by flick. Epic's output schemas are validated automatically.

## Relationship to Existing Issues

| Epic Issue | Status after upgrade |
|------------|---------------------|
| #8 (RunResult metadata discarded) | Resolved by changes 2-5 above |

## Note on lot Dependency

Epic's `Cargo.toml` currently uses `lot = { path = "../lot" }` (local override). This should be reverted to a pinned git rev before any release, independent of this upgrade.
