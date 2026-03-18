# Audit Plan (2026-03-18)

## File List

### Code Files (29)
1. `.github/workflows/ci.yml`
2. `Cargo.toml`
3. `lot-cli/Cargo.toml`
4. `lot-cli/src/main.rs`
5. `lot/Cargo.toml`
6. `lot/src/command.rs`
7. `lot/src/error.rs`
8. `lot/src/lib.rs`
9. `lot/src/linux/cgroup.rs`
10. `lot/src/linux/mod.rs`
11. `lot/src/linux/namespace.rs`
12. `lot/src/linux/seccomp.rs`
13. `lot/src/macos/mod.rs`
14. `lot/src/macos/seatbelt.rs`
15. `lot/src/policy.rs`
16. `lot/src/policy_builder.rs`
17. `lot/src/unix.rs`
18. `lot/src/windows/appcontainer.rs`
19. `lot/src/windows/cmdline.rs`
20. `lot/src/windows/elevation.rs`
21. `lot/src/windows/job.rs`
22. `lot/src/windows/mod.rs`
23. `lot/src/windows/nul_device.rs`
24. `lot/src/windows/pipe.rs`
25. `lot/src/windows/sentinel.rs`
26. `lot/src/windows/traverse_acl.rs`
27. `lot/tests/integration.rs`
28. `lot_project_assistant.nu`
29. `lot_shell.nu`

### Documentation Files (6)
1. `CLAUDE.md`
2. `README.md`
3. `docs/DESIGN.md`
4. `docs/STATUS.md`
5. `docs/ISSUES.md`
6. `prompts/project_assistant.md`

## Narrow-Lens Review Areas (9)

1. **Correctness** — Logic errors, off-by-one mistakes, race conditions, missing error handling, incorrect state transitions, broken invariants.
2. **Simplification** — Unnecessary complexity, redundant code, over-abstraction, things that could be expressed more directly.
3. **Testing & testability** — Missing test coverage for code paths, hard-to-test patterns, suggestions for test cases that would catch regressions. For existing tests: does each test actually verify what its name implies? Can the test fail?
4. **Historical cruft** — Stale comments, dead code, outdated references, leftover TODOs, artifacts from previous implementations that no longer apply.
5. **Separation of concerns** — Does each entity (function, type, section) in this file have a clear single responsibility? Are there things mixed together that should be split?
6. **Naming & responsibilities** — Do names (functions, variables, types, sections) accurately reflect contents and behavior? Are responsibilities clear from names?
7. **Placement** — Is anything in this file that belongs elsewhere? Is this file in the right location within the project structure?
8. **Documentation-implementation mismatch** — For code files: does this file's behavior match what project documentation says about it? For documentation files: are the claims, instructions, and descriptions in this document accurate and up-to-date with the actual implementation?
9. **Error handling** — Fail as early as possible, no silent failures. Every error must be surfaced: return it, report it, or fail a test. For tests specifically: tests must never succeed when an error occurs.

## Broad-Lens Review Areas (6)

1. **Correctness** — Cross-file logic errors, interface contract violations, inconsistent assumptions between modules.
2. **Simplification (design-level)** — Architectural over-engineering, unnecessary abstraction layers, overly complex module relationships.
3. **Separation of concerns** — Cross-file responsibility overlap, circular dependencies, unclear ownership boundaries.
4. **Naming & responsibilities** — Cross-file naming inconsistencies, same concept named differently in different files.
5. **Placement** — Files or modules in wrong part of structure, functionality that should be colocated but is scattered.
6. **Documentation-implementation mismatch** — Systemic divergence between documentation and implementation.

## Agent Count

- Code files: 29
- Documentation files: 6
- Total reviewable files: 35
- Narrow-lens agents: 35 × 9 = 315
- Broad-lens agents: 6
- Consolidation agent: 1
- Triage agent: 1
- **Total: 323 agents**

## Batching Strategy

Max 16 concurrent agents. Narrow-lens reviews launched in batches of 16 (~20 batches). Broad-lens reviews run as single batch of 6.
