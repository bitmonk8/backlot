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
|       +-- record.rs            -- Record operation implementation
|       +-- query.rs             -- Query operation implementation
|       +-- test_support.rs      -- Shared mock librarians for tests (cfg(test))
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
    +-- record.rs       -- record operation logic
    +-- query.rs        -- query operation logic (read-only)
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

**Agent invocation** -- Two traits abstract agent calls so that tests can substitute mocks without requiring real LLM calls: `DerivedProducer` (used by bootstrap and record, writes derived documents) and `QueryResponder` (used by query, read-only, returns a structured `QueryResult` parsed from the agent's JSON response). The production implementation (`ReelLibrarian`) implements both traits, holds a reference to the shared `Agent`, and configures each call with the appropriate model, grant, and write paths. The grant is `TOOLS`-only (read-only filesystem tools); reel automatically enables Write/Edit tools when `write_paths` is non-empty, so the agent can write only to the paths listed in `write_paths` (the `derived/` directory). Splitting the traits ensures each operation depends only on the capability it uses, and test mocks need not stub unrelated methods.

### Bootstrap Operation

The bootstrap operation (`bootstrap.rs`) is the first of four core operations. It converts raw requirements text into the initial document set:

1. Pre-condition check: fails with `AlreadyInitialized` if any of `CHANGELOG.md`, `raw/`, or `derived/` exist.
2. Creates `raw/` and `derived/` directories.
3. Writes requirements to `raw/REQUIREMENTS_1.md`.
4. Invokes the librarian to produce derived documents in `derived/`.
5. Validates derived documents (warnings only, does not fail the operation).
6. Appends a bootstrap changelog entry.

Partial failure semantics: if the librarian fails, raw documents remain on disk (no rollback) and the changelog entry is not written.

### Record Operation

The record operation (`record.rs`) writes new content into the vault and invokes the librarian to integrate it into derived documents. It supports two modes:

- **`RecordMode::New`** -- Creates version 1 of a new document series. Fails with `VersionConflict` if any versions already exist for the given name.
- **`RecordMode::Append`** -- Creates the next version in an existing series. Fails with `DocumentNotFound` if no prior versions exist.

Sequence:

1. Validates the name and writes content to `raw/NAME_N.md` via `write_raw_versioned`.
2. Snapshots derived documents (filename to content bytes) before librarian invocation.
3. Invokes the librarian with a record-specific prompt that instructs it to read the new raw document and integrate its content into derived documents.
4. Snapshots derived documents again and computes the set of created or modified files by comparing content.
5. Appends a record changelog entry listing the raw filename and modified derived filenames.
6. Returns `Vec<DocumentRef>` of modified derived documents.

The record prompt instructs the librarian to: read `derived/PROJECT.md` for orientation, apply a relevance filter (keep decisions/constraints/patterns, discard routine progress), follow the superseding rule (new info replaces outdated), and avoid restructuring (only add/update content relevant to the new information).

Partial failure semantics: same as bootstrap. If the librarian fails, the raw document remains on disk and no changelog entry is written.

### Query Operation

The query operation (`query.rs`) is the only read-only operation. It answers questions from the vault's knowledge base without modifying any files or appending changelog entries.

Sequence:

1. Builds a query-specific system prompt with a read-only scope restriction (no writes allowed).
2. Formats the user's question as a user message.
3. Invokes the librarian via `answer_query` with a `TOOLS`-only grant and empty `write_paths`.
4. The librarian reads `derived/PROJECT.md` for orientation, reads relevant documents, and returns a JSON response.
5. The JSON response is parsed into a `QueryResult` containing coverage assessment (`Full`/`Partial`/`None`), a synthesized answer, and supporting extracts with source references.

The query prompt uses a separate scope restriction block that explicitly prohibits writes. The `ReelLibrarian` implementation passes an empty `write_paths` vec, so reel does not enable Write/Edit tools.

Response parsing handles JSON wrapped in markdown code fences (```` ```json ... ``` ````) or bare JSON. The parser validates coverage values and extract structure, returning descriptive errors for malformed responses.

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

**Derived snapshots.** The `snapshot_derived()` method reads all files in `derived/` into a `HashMap<String, Vec<u8>>` (filename to content bytes). The `compute_changed()` free function compares two snapshots and returns `Vec<DocumentRef>` of created or modified files (deletions are excluded). Operations call `snapshot_derived()` before and after librarian invocation, then `compute_changed()` to detect changes.

**Version scanning.** Versions are determined by scanning the `raw/` directory for files matching `BASE_N.md`. This is simple and correct given the small expected document counts. The `write_raw_versioned` method handles both "new series" (version 1, fail if exists) and "append" (next version, fail if no prior) modes.

**Validation as warnings.** Derived document validation (filename pattern, `# ` title, `<!-- scope: ` comment) produces warnings rather than errors. The librarian is expected to self-correct on subsequent invocations.

**Testable librarian.** The `LibrarianInvoker` trait allows bootstrap tests to run without real LLM calls. Mock invokers write predetermined files to `derived/`, verifying the operation's pre/post-condition logic, changelog behavior, and partial failure semantics independently of the model.

**Single shared Agent.** Rather than creating a new reel `Agent` per operation call, `Vault` creates one at construction time. Since `ModelRegistry` and `ProviderRegistry` are not `Clone`, they are consumed once. Per-call differences (model name, system prompt, tool grants) are passed via `AgentRequestConfig`.
