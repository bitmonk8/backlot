# Epic — Rust AI Orchestration Framework

See [README.md](../README.md) for project overview, usage, architecture summary, and lineage.

This folder contains detailed design documents. The README is the primary entry point; these docs provide depth beyond what the README covers.

## Document Index

| Document | Contents |
|---|---|
| [Architecture](ARCHITECTURE.md) | System layers, module structure, data flow |
| [Task Model](TASK_MODEL.md) | Recursive problem-solver: assessment, leaf/branch paths, verification, recovery |
| [Agent Design](AGENT_DESIGN.md) | Agent orchestration, model selection, tool access, prompt design |
| [Flick Integration](FLICK_INTEGRATION.md) | Agent hosting via Flick library crate |
| [Document Store](DOCUMENT_STORE.md) | Centralized knowledge management, research service |
| [Verification](VERIFICATION.md) | Build/lint/test gates, review types, fix loops |
| [Configuration](CONFIGURATION.md) | Project-agnostic configuration: verification steps, model preferences, paths |
| [TUI Design](TUI_DESIGN.md) | Terminal interface: task tree, worklog, progress display |
| [Fix Loop Spec](FIX_LOOP_SPEC.md) | Fix loop after verification failure: leaf fix, branch fix, scope circuit breaker |
| [Sandboxing](SANDBOXING.md) | Two-layer sandboxing: VM/container guidance (security) + operational correctness (delegated to [lot](https://github.com/bitmonk8/lot)) |
| [Lot Spec](LOT_SPEC.md) | Design spec for the lot sandboxing library (separate repo) |
| [Open Questions](OPEN_QUESTIONS.md) | Design decisions record (all resolved) |
| [NuShell Migration](NUSHELL_MIGRATION.md) | Spec: replace POSIX sh with NuShell as sole shell runtime (persistent MCP session) |
| [Remove Unsandboxed](DE_UNSANDBOXED.md) | Spec: remove unsandboxed execution fallback |
| [Status](STATUS.md) | Current phase, milestones, next work candidates, decisions log |

## Reference Material

- `C:\UnitySrc\fds2\EPIC_DESIGN2.md` — The recursive problem-solver design document (authoritative design source)
- `C:\UnitySrc\fds2\tools\epic\` — fds2_epic Python implementation (reference implementation)
