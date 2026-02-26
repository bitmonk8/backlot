# Agent Design

## Model Selection

Assessment determines the executing model per-task. Other activities have fixed model assignments:

| Activity | Model | Rationale |
|---|---|---|
| Assessment | Haiku (escalate to Sonnet if uncertain) | Classification task |
| Leaf implementation | Assessment-selected (Haiku/Sonnet/Opus) | Matches problem difficulty |
| Branch design + decompose | Assessment-selected | Decomposition difficulty varies |
| Inter-subtask checkpoint | Haiku | Classification (proceed/adjust/escalate) |
| Recovery assessment | Opus | Requires strongest reasoning |
| Leaf verification | max(Haiku, implementing model) capped at Sonnet | Match implementation complexity |
| Branch verification | Sonnet | Cross-subtask reasoning |
| Document operations | Haiku | Lightweight, fast |
| Commit message generation | Haiku | Mechanical |

## Tool Access

Adapted from fds2_epic's IntFlag pattern. In Rust, use a bitflag crate or enum set:

```rust
bitflags! {
    struct Tools: u32 {
        const NONE    = 0;
        const READ    = 1 << 0;   // File read, glob, grep
        const WRITE   = 1 << 1;   // File edit, write
        const BASH    = 1 << 2;   // Shell execution
        const TASK    = 1 << 3;   // Sub-agent delegation
        const WEB     = 1 << 4;   // Web search/fetch

        const EXECUTE = Self::BASH.bits() | Self::TASK.bits();
        const EXPLORE = Self::READ.bits() | Self::EXECUTE.bits() | Self::WEB.bits();
        const MODIFY  = Self::READ.bits() | Self::WRITE.bits();
        const ALL     = Self::READ.bits() | Self::WRITE.bits() | Self::EXECUTE.bits() | Self::WEB.bits();
    }
}
```

Per-phase tool grants:

| Task Path | Phase | Tools | Purpose |
|---|---|---|---|
| Any | Assess | NONE | Pure structured output |
| Leaf | Implement | READ \| WRITE \| EXECUTE | Code changes |
| Leaf | Verify | NONE | Structured judgment |
| Branch | Design + Decompose | EXPLORE | Research, no writes |
| Branch | Verify | TASK | May spawn sub-agents for large diffs |

## Prompt Assembly

Each agent call assembles a prompt from:
1. **System prompt** — role, constraints, output format
2. **Structural map** — task position in tree, sibling context
3. **Phase-specific instructions** — what this call should accomplish
4. **Tool descriptions** — available tools for this phase
5. **Verification criteria** — success conditions

Research Service is exposed as a tool to the agent during implementation and design+decompose phases.

## Structured Output Extraction

fds2_epic uses a two-query split: primary query uses full model, secondary Haiku query parses structured output. Consider whether Rust's type system can improve this — e.g., defining response schemas that map to Rust structs via serde.

## Model Provider Abstraction

ZeroClaw supports multiple AI providers (OpenAI, Anthropic, custom endpoints). The agent layer should abstract over the provider, allowing model selection by capability tier (fast/balanced/strong) rather than specific model names, with configuration mapping tiers to concrete models.
