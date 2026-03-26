# Project Status

## Current Phase

**All four core operations and CLI implemented.**

## What Is Implemented

- Workspace structure: `vault` (lib) + `vault-cli` (bin)
- Dependency on `reel` via git rev (`a6be158`)
- CI pipeline (fmt, clippy, test, build on Linux/macOS/Windows)
- Storage layer (`vault/src/storage.rs`):
  - Directory structure management (`raw/`, `derived/`, `CHANGELOG.md`)
  - Existence checks for vault initialization
  - JSONL changelog append and read
  - Raw document write/read with `NAME_N.md` versioning
  - Name validation (`^[A-Z][A-Z0-9_]*[A-Z0-9]$`)
  - Derived document listing, filename validation, and content validation
  - Full document inventory (raw + derived with scope comments)
  - UTC timestamp generation
  - `snapshot_derived()`, `compute_changed()`, and `compute_deleted()` for before/after derived document comparison
- Public API (`vault/src/lib.rs`):
  - `Vault` struct with `new()` constructor
  - `Vault::bootstrap()`, `Vault::record()`, `Vault::query()`, `Vault::reorganize()` methods
  - `VaultEnvironment` and `VaultModels` configuration types
  - `VaultCreateError`, `BootstrapError`, `RecordError`, `QueryError`, `ReorganizeError` error types
  - `RecordMode` enum, `DocumentRef` re-export
  - `ReorganizeReport`, `QueryResult`, `Coverage`, `Extract` with serde `Serialize` for JSON output
  - `DerivedValidationWarning` re-exported for caller-side warning handling
- Prompts module (`vault/src/prompts.rs`):
  - Shared prompt blocks: core principle, document format, cross-references, scope restriction, document inventory
  - Bootstrap-specific prompt block
  - Record-specific prompt block (relevance filter, superseding rule, no restructuring)
  - Query-specific prompt block (read-only scope restriction)
  - Reorganize-specific prompt block (full restructuring, lifecycle triggers, prose tightening)
- Librarian module (`vault/src/librarian.rs`):
  - `DerivedProducer` and `QueryResponder` traits (split by operation type)
  - `ReelLibrarian` production implementation using reel `Agent`
  - `QueryResponder::answer_query`: read-only invocation with empty `write_paths`, JSON response parsing with markdown fence handling
- Bootstrap operation (`vault/src/bootstrap.rs`):
  - Pre-condition check (`AlreadyInitialized` if any prior state exists)
  - Directory creation, raw requirements write, librarian invocation
  - Post-invocation derived document validation (warnings only)
  - Changelog entry append on success
  - Partial failure semantics (raw persists, changelog skipped on librarian failure)
- Record operation (`vault/src/record.rs`):
  - Name validation and version assignment via `write_raw_versioned`
  - `RecordMode::New` (create series) and `RecordMode::Append` (next version)
  - Derived document snapshot before/after librarian invocation for change detection
  - Changelog entry with `derived_modified` list
  - Partial failure semantics (raw persists, changelog skipped on librarian failure)
- Query operation (`vault/src/query.rs`):
  - Read-only operation: no file writes, no changelog entry
  - `Coverage` enum (`Full`/`Partial`/`None`), `QueryResult`, `Extract` types
- Reorganize operation (`vault/src/reorganize.rs`):
  - Full restructuring pass over derived documents
  - Snapshot diffing to categorize changes as merged/restructured/deleted
  - Changelog entry with merged/restructured/deleted lists
  - `ReorganizeReport` return type
  - Partial failure semantics (changelog skipped on librarian failure)
- CLI (`vault-cli/src/main.rs`):
  - Four subcommands: `bootstrap`, `query`, `record`, `reorganize`
  - YAML configuration via `--config` flag
  - JSON output to stdout, error JSON to stderr
  - stdin reading for bootstrap requirements, query text, and record content
  - `--query` and `--content` flags for inline input
  - `--name` and `--mode` flags for record
  - Validation warning display (owned by CLI, not library)
  - Single-threaded tokio runtime
  - Unit tests for config parsing, mode conversion, error paths
- Shared test infrastructure (`vault/src/test_support.rs`):
  - `MockLibrarian`, `NoOpLibrarian`, `CapturingLibrarian`, `BadNameLibrarian`, `DeletingLibrarian`, `MockQueryLibrarian`
  - `DerivedWriter` type alias for configurable mock output
  - Each mock implements only its relevant trait (no dead stubs)
- Test coverage: 105 tests (storage, prompts, librarian, bootstrap, record, query, reorganize, Vault::new, CLI)

## What Remains

Nothing. All operations and CLI are implemented.
