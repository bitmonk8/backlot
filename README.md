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
epic    → cue, reel, vault, traits   Recursive problem-solver orchestrator
```

| Crate | Description | README |
|-------|-------------|--------|
| [flick](flick/) | Single-shot LLM primitive. Takes a request config + query, calls one model, returns JSON. Declares tools but never executes them — the caller drives the agent loop. Library + CLI. | [flick/README.md](flick/README.md) |
| [lot](lot/) | Cross-platform process sandboxing. Launches child processes with restricted filesystem and network access using namespaces+seccomp (Linux), Seatbelt (macOS), or AppContainer (Windows). Library + CLI. | [lot/README.md](lot/README.md) |
| [traits](traits/) | Shared trait definitions. Contains `EventEmitter<E>` for cross-crate event propagation without coupling emitters to consumers. | |
| [reel](reel/) | Agent session runtime. Owns the tool loop (50 rounds / 200 tool calls), a sandboxed NuShell MCP session via lot, and 6 built-in tools (Read, Write, Edit, Glob, Grep, NuShell). Library + CLI. | [reel/README.md](reel/README.md) |
| [vault](vault/) | Persistent file-based knowledge store. Accumulates project knowledge through four operations (bootstrap, record, query, reorganize) backed by a reel agent. Library + CLI. | [vault/README.md](vault/README.md) |
| [cue](cue/) | Generic recursive task orchestration framework. Defines the `TaskNode` and `TaskStore` traits, coordination algorithm (DFS traversal, retry, escalation, fix loops, recovery), `CueEvent` enum, and shared orchestration types. Orchestrator is generic over `EventEmitter<CueEvent>`. No AI dependencies. | |
| [epic](epic/) | Recursive problem-solver. Implements cue's `TaskNode` and `TaskStore` traits with AI agent calls (via reel), vault knowledge persistence, prompts, wire formats, `EventLog` (append-only event store with subscriptions), and TUI. | [epic/README.md](epic/README.md) |

## Documentation

Per-crate design documents, status tracking, and issue lists live under [`docs/`](docs/):

- [`FLICK_DESIGN.md`](docs/FLICK_DESIGN.md) — LLM provider abstraction design
- [`LOT_DESIGN.md`](docs/LOT_DESIGN.md) — Process sandboxing design
- [`REEL_DESIGN.md`](docs/REEL_DESIGN.md) — Agent runtime design
- [`VAULT_DESIGN.md`](docs/VAULT_DESIGN.md) — Document store design
- [`EPIC_DESIGN.md`](docs/EPIC_DESIGN.md) — Orchestrator design
- [`STATUS.md`](docs/STATUS.md) — Cross-crate status and next work
- [`ISSUES.md`](docs/ISSUES.md) — Tracked issues

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
