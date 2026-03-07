# Project Status

## Current Phase

**Audit remediation in progress** — All v1 features implemented (193 tests passing, 0 clippy warnings). Full codebase audit complete (95 review cells, 541 findings). Remediation ongoing; 3 groups remain.

## What Exists

- Recursive problem-solver orchestrator with DFS execution, retry/escalation, fix loops, recovery re-decomposition, checkpoint adjust/escalate
- `FlickAgent` implementing `AgentService` via Flick library crate — config generation, structured output schemas, prompt assembly, tool loop with resume
- 6 tools: `read_file`, `glob`, `grep`, `write_file`, `edit_file`, `bash` — path sandboxing, size limits, timeout handling, process group kill
- State persistence via `.epic/state.json` — atomic writes, resume, goal mismatch detection, corrupt state handling, cycle-safe DFS
- TUI via ratatui + crossterm — task tree, worklog, metrics panels, keyboard controls
- CLI via clap — `init`, `run <goal>`, `resume`, `status` subcommands
- `epic init` — agent-driven interactive configuration scaffolding
- Container/VM startup detection with suppressible warning
- CI pipeline — GitHub Actions (fmt, clippy, test, build), Rust 1.93.1 toolchain, Flick pinned to rev `8bf1d79`

## Design Choices (intentional constraints)

### Sequential execution only

Epic executes subtasks sequentially by design. Simplifies implementation, keeps TUI output and logging coherent, and prioritizes cost control and correctness over throughput while the design matures.

### No multi-language special handling

Epic uses generalized prompts that work across languages. No language-specific logic.

### No git hosting integration

No GitHub/GitLab PR creation, issue tracking, or similar integrations in v1.

## Next Work Candidates

Prioritized from audit findings (see [AUDIT.md](AUDIT.md#recommended-action-items-priority-order)):

1. **Config validation** (3 partial) — LimitsConfig bounds checking, PartialEq derives, load abstraction.
2. **Testability** (16 majors) — Injection seams, FS/process abstractions, mock sharing, missing test coverage. Largest group — incremental.
3. **Operational correctness sandboxing (Frida)** — TOCTOU mitigations + per-phase syscall enforcement. Deferred until 1–2 addressed. See [SANDBOXING.md](SANDBOXING.md).
