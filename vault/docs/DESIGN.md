# Design

Vault is a persistent, file-based knowledge store for agent systems. See [SPEC.md](SPEC.md) for the full specification.

## Project Structure

```
vault/                            (workspace root)
+-- Cargo.toml                   (workspace config, shared lints/versions/profile)
+-- vault/                       (library crate)
|   +-- Cargo.toml
|   +-- src/
|       +-- lib.rs               -- Public API: Vault, VaultEnvironment, error types
|       +-- storage.rs           -- Storage layer: file I/O, changelog, versioning
|       +-- prompts.rs           -- System prompt composition (shared + per-operation)
|       +-- librarian.rs         -- Agent invocation trait + ReelLibrarian impl
|       +-- bootstrap.rs         -- Bootstrap operation implementation
+-- vault-cli/                   (CLI binary crate)
|   +-- Cargo.toml
|   +-- src/
|       +-- main.rs              -- CLI: subcommands mapping to library API
+-- docs/
+-- .github/
```

## Dependencies

- **reel** -- agent session layer (git rev dependency)
- **serde** + **serde_json** -- changelog entry serialization (JSONL format)
- **thiserror** -- ergonomic error type derivation
- **regex** -- raw document name validation and version parsing
- **tempfile** (dev) -- temporary directories for unit tests
- **tokio** (dev) -- async runtime for bootstrap operation tests

## Architecture

### Layer Diagram

```
  Vault (lib.rs)        -- public API, owns Agent + Storage
    |
    +-- bootstrap.rs    -- bootstrap operation logic
    |     |
    |     +-- prompts.rs    -- system prompt composition (shared + per-operation)
    |     +-- librarian.rs  -- agent invocation trait + ReelLibrarian impl
    |
    +-- storage.rs      -- all file I/O primitives
```

### Vault Struct

`Vault` is the public entry point. It is constructed from a `VaultEnvironment` containing the storage root path, reel registries, and per-operation model names (`VaultModels`). At construction time it creates a reel `Agent` (consuming the registries) and a `Storage` handle. The agent is reused across operations; per-call configuration (model, prompt, grants) is passed via `AgentRequestConfig`.

### Prompts

The prompts module (`prompts.rs`) composes system prompts from shared blocks (core principle, document format, cross-references, scope restriction, document inventory) plus operation-specific blocks. Each operation has a dedicated prompt builder function.

### Librarian

The librarian (`librarian.rs`) is the interface between vault operations and the reel agent. It has one responsibility:

**Agent invocation** -- The `LibrarianInvoker` trait abstracts agent calls so that tests can substitute mocks without requiring real LLM calls. The production implementation (`ReelLibrarian`) holds a reference to the shared `Agent` and configures each call with the appropriate model, grant, and write paths. The grant is `TOOLS`-only (read-only filesystem tools); reel automatically enables Write/Edit tools when `write_paths` is non-empty, so the agent can write only to the paths listed in `write_paths` (the `derived/` directory). The trait method `produce_derived` names the side-effect: it reads raw documents and writes derived documents.

### Bootstrap Operation

The bootstrap operation (`bootstrap.rs`) is the first of four core operations. It converts raw requirements text into the initial document set:

1. Pre-condition check: fails with `AlreadyInitialized` if any of `CHANGELOG.md`, `raw/`, or `derived/` exist.
2. Creates `raw/` and `derived/` directories.
3. Writes requirements to `raw/REQUIREMENTS_1.md`.
4. Invokes the librarian to produce derived documents in `derived/`.
5. Validates derived documents (warnings only, does not fail the operation).
6. Appends a bootstrap changelog entry.

Partial failure semantics: if the librarian fails, raw documents remain on disk (no rollback) and the changelog entry is not written.

## Storage Layer

The storage layer (`vault/src/storage.rs`) is the foundational module that all vault operations depend on. It manages the on-disk directory layout and provides all file I/O primitives. It is internal to the vault crate (not part of the public API).

### Key Types

- **`Storage`** -- Main struct holding a `PathBuf` to the storage root. Provides methods for directory creation, existence checks, changelog operations, raw document I/O, derived document listing/validation, and document inventory.
- **`ChangelogEntry`** -- Tagged enum (`bootstrap` / `record` / `reorganize`) serialized as JSONL with serde. Each variant carries a UTC timestamp and operation-specific fields.
- **`DocumentRef`** -- Lightweight reference to a document by filename.
- **`RawDocumentVersion`** -- Parsed raw document with base name, version number, and filename.
- **`DerivedValidationWarning`** -- Warning produced when a derived document fails filename or header validation.
- **`DocumentInventory`** -- Combined listing of all raw and derived documents.

### Design Decisions

**Synchronous I/O.** The storage layer uses `std::fs` rather than `tokio::fs`. The spec states that access is serialized by the orchestrator, so there is no need for async file operations at this level. Async boundaries are introduced higher up (at the operation level) where the librarian agent is invoked.

**No external time crate.** UTC timestamps are computed from `std::time::SystemTime` using Hinnant's civil calendar algorithm. This avoids adding `chrono` or `time` as a dependency for a single formatting function.

**Regex-based validation.** Raw document names must match `^[A-Z][A-Z0-9_]*[A-Z0-9]$` (minimum 2 characters). Versioned filenames follow `NAME_N.md`. Derived filenames follow `NAME.md`. All patterns are compiled once via `LazyLock<Regex>`.

**Version scanning.** Versions are determined by scanning the `raw/` directory for files matching `BASE_N.md`. This is simple and correct given the small expected document counts. The `write_raw_versioned` method handles both "new series" (version 1, fail if exists) and "append" (next version, fail if no prior) modes.

**Validation as warnings.** Derived document validation (filename pattern, `# ` title, `<!-- scope: ` comment) produces warnings rather than errors. The librarian is expected to self-correct on subsequent invocations.

**Testable librarian.** The `LibrarianInvoker` trait allows bootstrap tests to run without real LLM calls. Mock invokers write predetermined files to `derived/`, verifying the operation's pre/post-condition logic, changelog behavior, and partial failure semantics independently of the model.

**Single shared Agent.** Rather than creating a new reel `Agent` per operation call, `Vault` creates one at construction time. Since `ModelRegistry` and `ProviderRegistry` are not `Clone`, they are consumed once. Per-call differences (model name, system prompt, tool grants) are passed via `AgentRequestConfig`.
