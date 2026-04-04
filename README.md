# Backlot

Rust AI agent harness — LLM abstraction, process sandboxing, tool-loop runtime, knowledge store, and recursive task orchestration.

## Crates

```
flick  (leaf)          LLM provider abstraction
lot    (leaf)          Cross-platform process sandboxing
reel   → flick, lot    Agent session runtime + tool loop
vault  → reel          File-based knowledge store
epic   → reel, vault   Recursive problem-solver orchestrator
```

| Crate | Description | README |
|-------|-------------|--------|
| [flick](flick/) | Single-shot LLM primitive. Takes a request config + query, calls one model, returns JSON. Declares tools but never executes them — the caller drives the agent loop. Library + CLI. | [flick/README.md](flick/README.md) |
| [lot](lot/) | Cross-platform process sandboxing. Launches child processes with restricted filesystem and network access using namespaces+seccomp (Linux), Seatbelt (macOS), or AppContainer (Windows). Library + CLI. | [lot/README.md](lot/README.md) |
| [reel](reel/) | Agent session runtime. Owns the tool loop (50 rounds / 200 tool calls), a sandboxed NuShell MCP session via lot, and 6 built-in tools (Read, Write, Edit, Glob, Grep, NuShell). Library + CLI. | [reel/README.md](reel/README.md) |
| [vault](vault/) | Persistent file-based knowledge store. Accumulates project knowledge through four operations (bootstrap, record, query, reorganize) backed by a reel agent. Library + CLI. | [vault/README.md](vault/README.md) |
| [epic](epic/) | Recursive problem-solver. Decomposes large tasks into subtasks, delegates to AI agents, verifies results, and recovers from failures. Uses vault for knowledge persistence and reel for agent sessions. | [epic/README.md](epic/README.md) |

## Documentation

Per-crate design documents, status tracking, and issue lists live under [`docs/`](docs/):

- [`docs/flick/`](docs/flick/) — Architecture, named models, status, issues
- [`docs/lot/`](docs/lot/) — Design, status, issues
- [`docs/reel/`](docs/reel/) — Design, status, issues
- [`docs/vault/`](docs/vault/) — Design, status, issues
- [`docs/epic/`](docs/epic/) — Design, orchestrator extraction spec, events, status, issues

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
