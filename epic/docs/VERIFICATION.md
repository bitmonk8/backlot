# Verification

## Configurable Verification Steps

Unlike fds2_epic which hardcodes `please.py build/lint/test`, verification steps are configured per-project. See [Configuration](CONFIGURATION.md) for the config format.

```rust
struct VerificationStep {
    name: String,           // "Build", "Lint", "Test"
    command: Vec<String>,   // e.g., ["cargo", "build"]
    timeout: u32,
}

struct VerificationConfig {
    steps: Vec<VerificationStep>,
}
```

## Leaf Verification

See [README.md](../README.md) for overview of fix loops and scope circuit breaker.

Model for leaf verification: `max(Haiku, implementing_model)` capped at Sonnet. See [FIX_LOOP_SPEC.md](FIX_LOOP_SPEC.md) for implementation details.

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
