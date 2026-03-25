# Project Status

## Current Phase

**Bootstrap and Record operations implemented. Two operations remaining.**

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
- Public API (`vault/src/lib.rs`):
  - `Vault` struct with `new()` constructor and `bootstrap()` method
  - `VaultEnvironment` and `VaultModels` configuration types
  - `VaultCreateError` and `BootstrapError` error types
- Prompts module (`vault/src/prompts.rs`):
  - System prompt composition: shared blocks (core principle, document format, cross-references, scope restriction, document inventory) plus bootstrap-specific block
- Librarian module (`vault/src/librarian.rs`):
  - `LibrarianInvoker` trait (`produce_derived`) for testable agent invocation
  - `ReelLibrarian` production implementation using reel `Agent`
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
- Public API additions:
  - `RecordMode` enum, `RecordError` error type, `DocumentRef` re-export
  - `Vault::record()` method
- Prompts module additions:
  - Record-specific prompt block (relevance filter, superseding rule, no restructuring)
  - `build_record_prompt()` and `record_query()` functions
- Storage additions:
  - `snapshot_derived()` and `compute_changed()` for before/after derived document comparison
- Shared test infrastructure (`vault/src/test_support.rs`):
  - `MockLibrarian`, `NoOpLibrarian`, `CapturingLibrarian`, `BadNameLibrarian`
  - `DerivedWriter` type alias for configurable mock output
- Test coverage: 65 tests (storage, prompts, bootstrap, record, Vault::new)

## What Remains

- Core operations: Query, Reorganize
- CLI subcommands
