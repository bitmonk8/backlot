# Backlot

Rust AI agent harness — LLM abstraction, process sandboxing, tool-loop runtime, knowledge store, and recursive task orchestration.

## Crates

```
flick   (leaf)                LLM provider abstraction
lot     (leaf)                Cross-platform process sandboxing
traits  (leaf)                Shared trait definitions (EventEmitter<E>)
reel    → flick, lot          Agent session runtime + tool loop
vault   → reel                File-based knowledge store
cue     → traits              Generic recursive task orchestration framework
mech    → cue, reel            Declarative YAML workflow format (in development)
epic    → cue, reel, vault, traits   Recursive problem-solver orchestrator
gate    (binary)              End-to-end test harness (manual)
```

| Crate | Description | README |
|-------|-------------|--------|
| [flick](flick/) | Single-shot LLM primitive. Takes a request config + query, calls one model, returns JSON. Declares tools but never executes them — the caller drives the agent loop. Library + CLI. | [flick/README.md](flick/README.md) |
| [lot](lot/) | Cross-platform process sandboxing. Launches child processes with restricted filesystem and network access using namespaces+seccomp (Linux), Seatbelt (macOS), or AppContainer (Windows). Library + CLI. | [lot/README.md](lot/README.md) |
| [traits](traits/) | Shared trait definitions. Contains `EventEmitter<E>` for cross-crate event propagation without coupling emitters to consumers. | |
| [reel](reel/) | Agent session runtime. Owns the tool loop (50 rounds / 200 tool calls), a sandboxed NuShell MCP session via lot, and 6 built-in tools (Read, Write, Edit, Glob, Grep, NuShell). Library + CLI. | [reel/README.md](reel/README.md) |
| [vault](vault/) | Persistent file-based knowledge store. Accumulates project knowledge through four operations (bootstrap, record, query, reorganize) backed by a reel agent. Library + CLI. | [vault/README.md](vault/README.md) |
| [cue](cue/) | Generic recursive task orchestration framework. Defines the `TaskNode` and `TaskStore` traits, coordination algorithm (DFS traversal, retry, escalation, fix loops, recovery), `CueEvent` enum, and shared orchestration types. Orchestrator is generic over `EventEmitter<CueEvent>`. No AI dependencies. | |
| [mech](mech/) | Declarative YAML workflow engine for LLM-driven control and dataflow. Prompt and call blocks in a unified CDFG, CEL expressions for guards and templates, reel agent execution, cue integration for retry and escalation. See `docs/MECH_SPEC.md` for the full specification. | [mech/README.md](mech/README.md) |
| [epic](epic/) | Recursive problem-solver. Implements cue's `TaskNode` and `TaskStore` traits with AI agent calls (via reel), vault knowledge persistence, prompts, wire formats, `EventLog` (append-only event store with subscriptions), and TUI. | [epic/README.md](epic/README.md) |
| [gate](gate/) | Manually-invoked end-to-end test harness exercising the full backlot stack against real LLM providers and OS sandboxing. Binary-only — no library component. See [docs/GATE_DESIGN.md](docs/GATE_DESIGN.md). | |

## Documentation

Per-crate design documents, status tracking, and issue lists live under [`docs/`](docs/):

- [`FLICK_DESIGN.md`](docs/FLICK_DESIGN.md) — LLM provider abstraction design
- [`LOT_DESIGN.md`](docs/LOT_DESIGN.md) — Process sandboxing design
- [`REEL_DESIGN.md`](docs/REEL_DESIGN.md) — Agent runtime design
- [`VAULT_DESIGN.md`](docs/VAULT_DESIGN.md) — Document store design
- [`EPIC_DESIGN.md`](docs/EPIC_DESIGN.md) — Orchestrator design
- [`GATE_DESIGN.md`](docs/GATE_DESIGN.md) — End-to-end test harness design
- [`STATUS.md`](docs/STATUS.md) — Cross-crate status and next work


## Build

Requires Rust 1.85+ (edition 2024).

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
