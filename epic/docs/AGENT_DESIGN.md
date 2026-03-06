# Agent Design

See [README.md](../README.md) for overview of model selection and tool access.

This document covers per-phase tool grants, prompt assembly, and structured output details not in the README.

## Per-Phase Tool Grants

| Task Path | Phase | Tools | Purpose |
|---|---|---|---|
| Any | Assess | READ | Read-only analysis |
| Leaf | Implement | READ \| WRITE \| EXECUTE | Code changes |
| Leaf | Verify | READ | Read-only analysis |
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

## Structured Output

Flick returns structured JSON via `output_schema` configuration. Epic deserializes the response text into wire format types (e.g., `AssessmentWire`, `CheckpointWire`) via serde, then converts to domain types via `TryFrom`. See [Flick Integration](FLICK_INTEGRATION.md).
