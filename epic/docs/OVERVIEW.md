# Epic — Rust AI Orchestration Framework

## Purpose

Epic is an AI agent orchestration tool that decomposes large software engineering tasks into smaller problems, delegates them to AI agents, and assembles verified results. It is a ground-up Rust reimplementation informed by the Python-based `fds2_epic` tool, but with different architectural choices.

## Lineage and Divergence

Epic inherits the *conceptual model* from fds2_epic but diverges in implementation:

| Concern | fds2_epic | This project |
|---|---|---|
| Language | Python | Rust |
| Task model | Six task types (GROUP, RESEARCH, DESIGN, PLAN, IMPLEMENTATION, VERIFY) transitioning to recursive solver | Recursive problem-solver exclusively (EPIC_DESIGN2) |
| Agent sandboxing | Custom command filtering, per-session config isolation | ZeroClaw as agent host (native sandboxing) |
| Project scope | Hardcoded for fds2 (build/lint/test via `please.py`) | Generalized — configurable verification for any project |
| TUI | Python Textual | Rust TUI (ratatui or similar) |

## Key Design Decisions

1. **Recursive problem-solver only** — No legacy task types. Every task follows: assess → execute (leaf or branch) → verify. See [Task Model](TASK_MODEL.md).
2. **ZeroClaw as agent host** — Agent sandboxing, filesystem scoping, and process isolation delegated to ZeroClaw rather than reimplemented. See [ZeroClaw Integration](ZEROCLAW_INTEGRATION.md).
3. **Configurable verification** — Build/lint/test commands specified per-project via configuration, not hardcoded. See [Configuration](CONFIGURATION.md).
4. **Rust for performance and type safety** — CLI/TUI responsiveness, strong static typing for orchestration correctness, and better agent SDK ergonomics.

## Document Index

| Document | Contents |
|---|---|
| [Architecture](ARCHITECTURE.md) | System layers, module structure, data flow |
| [Task Model](TASK_MODEL.md) | Recursive problem-solver: assessment, leaf/branch paths, verification, recovery |
| [Agent Design](AGENT_DESIGN.md) | Agent orchestration, model selection, tool access, prompt design |
| [ZeroClaw Integration](ZEROCLAW_INTEGRATION.md) | Sandboxing, agent hosting, capability scoping via ZeroClaw |
| [Document Store](DOCUMENT_STORE.md) | Centralized knowledge management, research service |
| [Verification](VERIFICATION.md) | Build/lint/test gates, review types, fix loops |
| [Configuration](CONFIGURATION.md) | Project-agnostic configuration: verification steps, model preferences, paths |
| [TUI Design](TUI_DESIGN.md) | Terminal interface: task tree, worklog, progress display |
| [Open Questions](OPEN_QUESTIONS.md) | Unresolved design decisions requiring investigation |
| [ZeroClaw Audit Plan](ZEROCLAW_AUDIT_PLAN.md) | Security audit plan: 14 audit units covering all ZeroClaw modules |
| [Audit Summary](audit/SUMMARY.md) | Security audit results: CONDITIONAL PASS, no backdoors found |
| [Status](STATUS.md) | Current phase, milestones, next work candidates, decisions log |

## Reference Material

- `C:\UnitySrc\fds2\EPIC_DESIGN2.md` — The recursive problem-solver design document (authoritative design source)
- `C:\UnitySrc\fds2\tools\epic\` — fds2_epic Python implementation (reference implementation)
- [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) — Agent hosting platform

## Status

**Phase: Design / Research / Specification**
