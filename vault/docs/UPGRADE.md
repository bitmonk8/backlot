# Vault Upgrade: Reel Observability Fields

## Context

Reel (rev `93f35ef`, already pinned in vault's Cargo.toml) now exposes three new data points in `RunResult`:

1. **Session transcript** — `Vec<TurnRecord>` capturing every tool call, per-turn usage, and API latency across the agent session.
2. **Cache token fields** — `cache_creation_input_tokens` and `cache_read_input_tokens` in the `Usage` struct. These reflect Anthropic prompt caching behavior enabled by flick's new 2-breakpoint cache strategy.
3. **Per-call API latency** — `api_latency_ms` available both in `Usage` (session total) and per-turn in the transcript.

Vault's reel dependency already provides all of this. No dependency version bump is needed.

## What Needs to Change

### 1. Capture RunResult from librarian sessions

Both `DerivedProducer` and `QueryResponder` currently discard the `RunResult` after extracting either nothing (write ops) or `.output` (query). The full `RunResult` — including `usage`, `tool_calls`, and `transcript` — must be returned from librarian calls so vault can surface it.

### 2. Thread session metadata through operation results

Each vault operation (bootstrap, record, query, reorganize) returns a domain-specific result today. After the librarian returns session metadata, each operation must include it in its return value so the CLI layer can access it.

### 3. Include usage and timing in CLI JSON output

Every vault CLI command should include a `usage` block in its JSON output. At minimum:

- `input_tokens`
- `output_tokens`
- `cache_creation_input_tokens` (omit if zero)
- `cache_read_input_tokens` (omit if zero)
- `cost_usd` (if available)
- `api_latency_ms`
- `tool_calls` (count)

This enables callers (including rig's test harness) to verify budget expectations and cache effectiveness per operation.

### 4. Expose session transcript (optional but recommended)

The transcript captures the full agent decision trail — which tools were called, what was read/written, and how many turns were needed. For vault this is valuable for diagnosing librarian behavior (e.g., "why did reorganize delete that document?").

Options:
- Include in CLI JSON output under a `transcript` key (verbose but complete).
- Write to a sidecar file (e.g., `storage_root/transcripts/<operation>_<timestamp>.json`).
- Expose only on `--verbose` flag.

## What Does NOT Need to Change

- **Prompt caching** works automatically. Flick injects `cache_control` on every Messages API call. Vault gets faster/cheaper librarian sessions with no code changes.
- **Structured output validation** (fence stripping, schema checks) is handled internally by flick. No vault changes needed.

## Relationship to Existing Findings

| Finding | Status after upgrade |
|---------|---------------------|
| F-002 (bootstrap requires pre-existing storage_root) | Unrelated — still needs a separate fix |
| F-003 (no token usage in CLI output) | Resolved by changes 2 and 3 above |
| F-005 (discards usage/cost from reel) | Resolved by change 1 above |
