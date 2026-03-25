# Project Status

## Current Phase

**Bootstrap, Record, and Query operations implemented. Reorganize remaining.**

## What Is Implemented

- Workspace structure: `vault` (lib) + `vault-cli` (bin)
- Dependency on `reel` via git rev (`a6be158`)
- CI pipeline (fmt, clippy, test, build on Linux/macOS/Windows)
- Project specification ([SPEC.md](SPEC.md))
- Storage layer (`vault/src/storage.rs`):
  - Directory structure management (`raw/`, `derived/`, `CHANGELOG.md`)
  - Existence checks for vault initialization
  - JSONL changelog append and read
  - Raw document write/read with `NAME_N.md` versioning
  - Name validation (`^[A-Z][A-Z0-9_]*[A-Z0-9]$`)
  - Derived document listing, filename validation, and content validation
  - Full document inventory (raw + derived)
  - UTC timestamp generation
  - `snapshot_derived()` and `compute_changed()` for before/after derived document comparison
- Public API (`vault/src/lib.rs`):
  - `Vault` struct with `new()` constructor
  - `Vault::bootstrap()`, `Vault::record()`, `Vault::query()` methods
  - `VaultEnvironment` and `VaultModels` configuration types
  - `VaultCreateError`, `BootstrapError`, `RecordError`, `QueryError` error types
  - `RecordMode` enum, `DocumentRef` re-export
- Prompts module (`vault/src/prompts.rs`):
  - Shared prompt blocks: core principle, document format, cross-references, scope restriction, document inventory
  - Bootstrap-specific prompt block
  - Record-specific prompt block (relevance filter, superseding rule, no restructuring); `build_record_prompt()`, `record_query()`
  - Query-specific prompt block (read-only scope restriction); `build_query_prompt()`, `query_user_message()`
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
- Shared test infrastructure (`vault/src/test_support.rs`):
  - `MockLibrarian`, `NoOpLibrarian`, `CapturingLibrarian`, `BadNameLibrarian`, `MockQueryLibrarian`
  - `DerivedWriter` type alias for configurable mock output
  - Each mock implements only its relevant trait (no dead stubs)
- Test coverage: 84 tests (storage, prompts, librarian, bootstrap, record, query, Vault::new)

## What Remains

- Core operations: Reorganize
- CLI subcommands
