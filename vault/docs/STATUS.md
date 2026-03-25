# Project Status

## Current Phase

**Bootstrap operation implemented. Three operations remaining.**

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
- Test coverage: 46 tests (storage, prompts, bootstrap operation, Vault::new)

## What Remains

- Core operations: Query, Record, Reorganize
- CLI subcommands
