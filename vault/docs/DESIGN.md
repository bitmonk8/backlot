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
|       +-- reorganize.rs       -- Reorganize operation implementation
|       +-- test_support.rs      -- Shared mock librarians for tests (cfg(test))
+-- vault-cli/                   (CLI binary crate)
|   +-- Cargo.toml
|   +-- src/
|       +-- main.rs              -- CLI: subcommands mapping to library API
+-- docs/
+-- .github/
```

## Dependencies

### vault (library)

- **reel** -- agent session layer (git rev dependency)
- **serde** + **serde_json** -- changelog entry serialization (JSONL format), JSON output for CLI
- **thiserror** -- ergonomic error type derivation
- **regex** -- raw document name validation and version parsing
- **tempfile** (dev) -- temporary directories for unit tests
- **tokio** (dev) -- async runtime for operation tests

### vault-cli (binary)

- **vault** -- library crate (path dependency)
- **reel** -- constructs `ModelRegistry` and `ProviderRegistry`
- **clap** -- argument parsing with derive macros
- **serde** + **serde_json** -- JSON output serialization
- **serde_yaml** -- YAML configuration parsing
- **tokio** -- async runtime (single-threaded)

## Architecture

### Layer Diagram

```
  Vault (lib.rs)        -- public API, owns Agent + Storage
    |
    +-- bootstrap.rs    -- bootstrap operation logic
    +-- record.rs       -- record operation logic
    +-- query.rs        -- query operation logic (read-only)
    +-- reorganize.rs   -- reorganize operation logic (full sweep)
    |     |
    |     +-- prompts.rs    -- system prompt composition (shared + per-operation)
    |     +-- librarian.rs  -- agent invocation trait + ReelLibrarian impl
    |
    +-- storage.rs      -- all file I/O primitives
```

### Vault Struct

`Vault` is the public entry point. It is constructed from a `VaultEnvironment` containing the storage root path, reel registries, and per-operation model names (`VaultModels`). At construction time it creates a reel `Agent` (consuming the registries) and a `Storage` handle. The agent is reused across operations; per-call configuration (model, prompt, grants) is passed via `AgentRequestConfig`.

**Warning handling:** Operations that invoke the librarian (bootstrap, record, reorganize) perform post-invocation validation of derived documents. Validation warnings are returned to the caller as `Vec<DerivedValidationWarning>` — the library does not print them. The CLI is responsible for formatting and displaying warnings.

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

### Reorganize Operation

The reorganize operation (`reorganize.rs`) performs a full restructuring pass over derived documents. Unlike the lightweight librarian pass on each record, this is a thorough sweep that can merge, split, deduplicate, and tighten documents.

Sequence:

1. Snapshots derived documents before librarian invocation.
2. Builds a reorganize-specific system prompt instructing the librarian to review all derived documents for restructuring opportunities.
3. Invokes the librarian via `DerivedProducer::produce_derived`.
4. Validates derived documents (warnings only).
5. Snapshots derived documents after invocation and categorizes changes:
   - **merged**: files present in both snapshots where content changed (content was consolidated/updated).
   - **restructured**: files in the after snapshot but not in before (new documents from splits).
   - **deleted**: files in the before snapshot but not in after (removed during merge).
6. Appends a reorganize changelog entry with merged/restructured/deleted lists.
7. Returns a `ReorganizeReport`.

The reorganize prompt instructs the librarian to: read `derived/PROJECT.md` for orientation, apply document lifecycle triggers (size ~200 lines, coherence), merge overlapping documents, split multi-topic documents, remove duplicated content, tighten prose, and update the PROJECT.md index. The librarian may read `raw/` for content accuracy verification.

Partial failure semantics: if the librarian fails, no changelog entry is written. Derived documents may be in a partially modified state (same as other write operations).

## CLI

The `vault-cli` crate (`vault-cli/src/main.rs`) provides a command-line interface that maps directly to vault's public API. Uses clap 4 with derive macros, YAML configuration, JSON output to stdout, errors as JSON to stderr.

### Configuration

A YAML config file maps to `VaultEnvironment`. The CLI constructs `ModelRegistry` and `ProviderRegistry` internally via `load_default()`.

```yaml
storage_root: ".epic/docs/"
models:
  bootstrap: "sonnet"
  query: "haiku"
  record: "haiku"
  reorganize: "sonnet"
```

### Subcommands

- `vault bootstrap --config <path>` -- reads requirements from stdin, calls `Vault::bootstrap()`.
- `vault query --config <path> [--query <text>]` -- query text via `--query` flag or stdin, outputs `QueryResult` as JSON.
- `vault record --config <path> --name <NAME> --mode new|append [--content <text>]` -- content via `--content` flag or stdin, outputs `Vec<DocumentRef>` as JSON.
- `vault reorganize --config <path>` -- outputs `ReorganizeReport` as JSON.

## Storage Layer

The storage layer (`vault/src/storage.rs`) is the foundational module that all vault operations depend on. It manages the on-disk directory layout and provides all file I/O primitives. It is internal to the vault crate (not part of the public API).

### Key Types

- **`Storage`** -- Main struct holding a `PathBuf` to the storage root. Provides methods for directory creation, existence checks, changelog operations, raw document I/O, derived document listing/validation, and document inventory.
- **`ChangelogEntry`** -- Tagged enum (`bootstrap` / `record` / `reorganize`) serialized as JSONL with serde. Each variant carries a UTC timestamp and operation-specific fields.
- **`DocumentRef`** -- Lightweight reference to a document by filename.
- **`RawDocumentVersion`** -- Parsed raw document with base name, version number, and filename.
- **`DerivedValidationWarning`** -- Warning produced when a derived document fails filename or header validation.
- **`DerivedDocumentInfo`** -- A derived document filename paired with its optional scope comment (extracted from the `<!-- scope: ... -->` line).
- **`DocumentInventory`** -- Combined listing of all raw documents and derived documents with scope comments. The prompt block renders scope text alongside each derived entry.

### Design Decisions

**Synchronous I/O.** The storage layer uses `std::fs` rather than `tokio::fs`. The spec states that access is serialized by the orchestrator, so there is no need for async file operations at this level. Async boundaries are introduced higher up (at the operation level) where the librarian agent is invoked.

**No external time crate.** UTC timestamps are computed from `std::time::SystemTime` using Hinnant's civil calendar algorithm. This avoids adding `chrono` or `time` as a dependency for a single formatting function.

**Regex-based validation.** Raw document names must match `^[A-Z][A-Z0-9_]*[A-Z0-9]$` (minimum 2 characters). Versioned filenames follow `NAME_N.md`. Derived filenames follow `NAME.md`. All patterns are compiled once via `LazyLock<Regex>`.

**Derived snapshots.** The `snapshot_derived()` method reads all files in `derived/` into a `HashMap<String, Vec<u8>>` (filename to content bytes). The `compute_changed()` free function compares two snapshots and returns `Vec<DocumentRef>` of created or modified files. The `compute_deleted()` free function returns files present in the before snapshot but absent in the after snapshot. Operations call `snapshot_derived()` before and after librarian invocation; record uses `compute_changed()`, reorganize uses both `compute_changed()` and `compute_deleted()` to categorize changes into merged/restructured/deleted.

**Version scanning.** Versions are determined by scanning the `raw/` directory for files matching `BASE_N.md`. This is simple and correct given the small expected document counts. The `write_raw_versioned` method handles both "new series" (version 1, fail if exists) and "append" (next version, fail if no prior) modes.

**Validation as warnings.** Derived document validation (filename pattern, `# ` title, `<!-- scope: ` comment) produces warnings rather than errors. The librarian is expected to self-correct on subsequent invocations.

**Testable librarian.** The `LibrarianInvoker` trait allows bootstrap tests to run without real LLM calls. Mock invokers write predetermined files to `derived/`, verifying the operation's pre/post-condition logic, changelog behavior, and partial failure semantics independently of the model.

**Single shared Agent.** Rather than creating a new reel `Agent` per operation call, `Vault` creates one at construction time. Since `ModelRegistry` and `ProviderRegistry` are not `Clone`, they are consumed once. Per-call differences (model name, system prompt, tool grants) are passed via `AgentRequestConfig`.
