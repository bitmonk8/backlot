# Design

Vault is a persistent, file-based knowledge store for agent systems. See [SPEC.md](SPEC.md) for the full specification.

## Project Structure

```
vault/                            (workspace root)
├── Cargo.toml                   (workspace config, shared lints/versions/profile)
├── vault/                       (library crate)
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── vault-cli/                   (CLI binary crate)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs              — CLI: subcommands mapping to library API
├── docs/
└── .github/
```

## Dependencies

- **reel** — agent session layer (git rev dependency)
