# Project Status

## Current Phase

**Scaffold complete** — Project structure, CI, and spec are in place. No platform implementations yet.

## What Exists

- Project skeleton with platform-gated modules (linux, macos, windows)
- Public API types: `SandboxPolicy`, `SandboxCommand`, `SandboxError`, `PlatformCapabilities`
- `probe()` and `spawn()` entry points (stubs)
- CI pipeline: clippy + test + build on all three platforms
- Design spec: `docs/SPEC.md`

## Implementation Plan

See `docs/PLAN.md` for the full phased plan with CI testing strategy.

### Phase Summary

| Phase | Scope | Status |
|---|---|---|
| 0 | Policy validation + test infrastructure | Not started |
| 1 | `probe()` implementations (all platforms) | Not started |
| 2 | Windows Job Objects | Not started |
| 3 | Windows AppContainer | Not started |
| 4 | Linux seccomp-BPF | Not started |
| 5 | Linux namespaces + filesystem | Not started |
| 6 | Linux cgroups v2 | Not started |
| 7 | macOS Seatbelt | Not started |
| 8 | Integration + hardening | Not started |

### Next Work

Phase 0 (policy validation) — no platform dependencies, testable everywhere.
