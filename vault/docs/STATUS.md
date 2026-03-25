# Project Status

## Current Phase

**Spec complete. No functionality implemented yet.**

## What Is Implemented

- Workspace structure: `vault` (lib) + `vault-cli` (bin)
- Dependency on `reel` via git rev
- CI pipeline (fmt, clippy, test, build on Linux/macOS/Windows)
- Project specification ([SPEC.md](SPEC.md))

## What Remains

- Resolve open questions in SPEC.md
- Storage layer (file-based markdown read/write)
- Core operations: Bootstrap, Query, Record
- Librarian agent (model-configurable, document organization)
