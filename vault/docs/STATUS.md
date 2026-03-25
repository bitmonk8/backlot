# Project Status

## Current Phase

**Storage layer implemented. Operations not yet implemented.**

## What Is Implemented

- Workspace structure: `vault` (lib) + `vault-cli` (bin)
- Dependency on `reel` via git rev (`e9215a6`)
- CI pipeline (fmt, clippy, test, build on Linux/macOS/Windows)
- Project specification ([SPEC.md](SPEC.md))
- Storage layer (`vault/src/storage.rs`):
  - Directory structure management (`raw/`, `derived/`, `CHANGELOG.md`)
  - Existence checks for vault initialization
  - JSONL changelog append and read
  - Raw document write/read with `NAME_N.md` versioning
  - Name validation (`^[A-Z][A-Z0-9_]*[A-Z0-9]$`)
  - Derived document listing, filename validation, and content validation (warns on unreadable files; checks title and scope independently)
  - Full document inventory (raw + derived)
  - UTC timestamp generation
  - Unit tests covering storage functionality
  - Additional test coverage: `read_raw` not-found path, `read_changelog` corrupt data, `list_derived` direct tests, `inventory` with non-matching raw files, `create_directories` failure path

## What Remains

- Core operations: Bootstrap, Query, Record, Reorganize
- Librarian agent (model-configurable, document organization)
- CLI subcommands
