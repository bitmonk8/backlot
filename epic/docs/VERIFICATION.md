# Verification

## Configurable Verification Steps

Unlike fds2_epic which hardcodes `please.py build/lint/test`, verification steps are configured per-project. See [Configuration](CONFIGURATION.md) for the config format.

```rust
struct VerificationStep {
    name: String,           // "Build", "Lint", "Test"
    command: Vec<String>,   // e.g., ["cargo", "build"]
    timeout_secs: u64,
}

struct VerificationConfig {
    steps: Vec<VerificationStep>,
}
```

## Leaf Verification

Leaf tasks produce code changes. Verification is concrete and mechanical:

1. **Build/lint/test gates** — Run all configured verification steps sequentially
2. **File-level review** — Agent reviews actual source files against intended changes
3. **Local simplification review** — Spot redundancy, cleanup opportunities in touched files
4. **Fix loop** — On failure: fix → re-verify (3 retries per model tier)

Model for leaf verification: `max(Haiku, implementing_model)` capped at Sonnet.

### Fix Loop

- 3 retries per model tier (4 total attempts: 1 initial + 3 retries)
- Each attempt sees previous failures
- On exhaustion: escalate to next model tier (Haiku→Sonnet→Opus)
- Terminal failure at Opus: rollback all changes, fail to parent

### Scope Circuit Breaker

Parent sets magnitude estimate (max lines modified/added/deleted, +50% conservative).
During implementation, measure actual diff via `git diff --numstat`.
If any metric exceeds 3x estimate: immediate rollback, SCOPE_EXCEEDED failure.

## Branch Verification

Branch tasks produce no code directly. After all subtasks complete:

1. **Correctness review** — Does aggregate result satisfy parent's objectives? Interface compatibility between siblings?
2. **Completeness review** — Did sub-problems cover the whole problem? Any gaps?
3. **Aggregate simplification review** — Cross-file/cross-subtask redundancy, over-engineering?

Model: Sonnet. May spawn sub-agents (Task tool) for large diffs.

Up to 3 fix rounds. Creates additional subtasks to address issues.
Root gets one additional Opus round after 3 Sonnet rounds.

## Error Handling

- Error deduplication: group by pattern, max 3 per group, 50 lines total
- Regression detection: did fix introduce new failures?
- Build check is fail-fast (run before other verification)
