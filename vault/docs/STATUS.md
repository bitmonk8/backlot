# Project Status

## Current Phase

**Spec complete. No functionality implemented yet.**

## What Is Implemented

- Workspace structure: `vault` (lib) + `vault-cli` (bin)
- Dependency on `reel` via git rev (`e9215a6`)
- CI pipeline (fmt, clippy, test, build on Linux/macOS/Windows)
- Project specification ([SPEC.md](SPEC.md))
- Reel fine-grained path grants (blocking dependency resolved)

## What Remains

- Storage layer (file-based markdown read/write)
- Core operations: Bootstrap, Query, Record, Reorganize
- Librarian agent (model-configurable, document organization)
- CLI subcommands
