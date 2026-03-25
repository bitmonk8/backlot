# Design

Vault is a persistent, file-based knowledge store for agent systems. See [SPEC.md](SPEC.md) for the full specification.

## Project Structure

```
vault/                            (workspace root)
├── Cargo.toml                   (workspace config, shared lints/versions/profile)
├── vault/                       (library crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       └── storage.rs           — Storage layer: file I/O, changelog, versioning
├── vault-cli/                   (CLI binary crate)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs              — CLI: subcommands mapping to library API
├── docs/
└── .github/
```

## Dependencies

- **reel** — agent session layer (git rev dependency)
- **serde** + **serde_json** — changelog entry serialization (JSONL format)
- **thiserror** — ergonomic error type derivation
- **regex** — raw document name validation and version parsing
- **tempfile** (dev) — temporary directories for unit tests

## Storage Layer

The storage layer (`vault/src/storage.rs`) is the foundational module that all vault operations depend on. It manages the on-disk directory layout and provides all file I/O primitives. It is internal to the vault crate (not part of the public API).

### Key Types

- **`Storage`** — Main struct holding a `PathBuf` to the storage root. Provides methods for directory creation, existence checks, changelog operations, raw document I/O, derived document listing/validation, and document inventory.
- **`ChangelogEntry`** — Tagged enum (`bootstrap` / `record` / `reorganize`) serialized as JSONL with serde. Each variant carries a UTC timestamp and operation-specific fields.
- **`DocumentRef`** — Lightweight reference to a document by filename.
- **`RawDocumentVersion`** — Parsed raw document with base name, version number, and filename.
- **`DerivedValidationWarning`** — Warning produced when a derived document fails filename or header validation.
- **`DocumentInventory`** — Combined listing of all raw and derived documents.

### Design Decisions

**Synchronous I/O.** The storage layer uses `std::fs` rather than `tokio::fs`. The spec states that access is serialized by the orchestrator, so there is no need for async file operations at this level. Async boundaries are introduced higher up (at the operation level) where the librarian agent is invoked.

**No external time crate.** UTC timestamps are computed from `std::time::SystemTime` using Hinnant's civil calendar algorithm. This avoids adding `chrono` or `time` as a dependency for a single formatting function.

**Regex-based validation.** Raw document names must match `^[A-Z][A-Z0-9_]*[A-Z0-9]$` (minimum 2 characters). Versioned filenames follow `NAME_N.md`. Derived filenames follow `NAME.md`. All patterns are compiled once via `LazyLock<Regex>`.

**Version scanning.** Versions are determined by scanning the `raw/` directory for files matching `BASE_N.md`. This is simple and correct given the small expected document counts. The `write_raw_versioned` method handles both "new series" (version 1, fail if exists) and "append" (next version, fail if no prior) modes.

**Validation as warnings.** Derived document validation (filename pattern, `# ` title, `<!-- scope: ` comment) produces warnings rather than errors. The librarian is expected to self-correct on subsequent invocations.
