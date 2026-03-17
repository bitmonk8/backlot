# Audit Plan

## File List

### Code Files (24)
1. `.github/workflows/ci.yml`
2. `Cargo.toml`
3. `Cargo.lock`
4. `lot-cli/Cargo.toml`
5. `lot-cli/src/main.rs`
6. `lot/Cargo.toml`
7. `lot/src/command.rs`
8. `lot/src/error.rs`
9. `lot/src/lib.rs`
10. `lot/src/linux/cgroup.rs`
11. `lot/src/linux/mod.rs`
12. `lot/src/linux/namespace.rs`
13. `lot/src/linux/seccomp.rs`
14. `lot/src/macos/mod.rs`
15. `lot/src/macos/seatbelt.rs`
16. `lot/src/policy.rs`
17. `lot/src/policy_builder.rs`
18. `lot/src/unix.rs`
19. `lot/src/windows/appcontainer.rs`
20. `lot/src/windows/job.rs`
21. `lot/src/windows/mod.rs`
22. `lot/src/windows/nul_device.rs`
23. `lot/src/windows/traverse_acl.rs`
24. `lot/tests/integration.rs`
25. `rust-toolchain.toml`

### Documentation Files (8)
1. `.gitattributes`
2. `.gitignore`
3. `CLAUDE.md`
4. `LICENSE`
5. `README.md`
6. `docs/DESIGN.md`
7. `docs/ISSUES.md`
8. `docs/STATUS.md`
9. `prompts/project_assistant.md`

### Script Files (2)
1. `lot_project_assistant.nu`
2. `lot_shell.nu`

## Narrow-Lens Review Areas (9)

1. **Correctness** — Logic errors, off-by-one mistakes, race conditions, missing error handling, incorrect state transitions, broken invariants.
2. **Simplification** — Unnecessary complexity, redundant code, over-abstraction, things that could be expressed more directly.
3. **Testing & testability** — Missing test coverage, hard-to-test patterns, test validity.
4. **Historical cruft** — Stale comments, dead code, outdated references, leftover TODOs.
5. **Separation of concerns** — Single responsibility violations, mixed concerns.
6. **Naming & responsibilities** — Name accuracy, clarity of responsibilities.
7. **Placement** — Misplaced code or files.
8. **Documentation-implementation mismatch** — Accuracy of docs vs code.
9. **Error handling** — Early failure, no silent failures, error surfacing.

## Broad-Lens Review Areas (6)

1. **Correctness** — Cross-file logic errors, interface contract violations.
2. **Simplification (design-level)** — Architectural over-engineering, unnecessary abstractions.
3. **Separation of concerns** — Cross-file responsibility overlap, circular dependencies.
4. **Naming & responsibilities** — Cross-file naming inconsistencies.
5. **Placement** — Architectural layer violations, misplaced modules.
6. **Documentation-implementation mismatch** — Systemic doc/code divergence.

## Agent Count

- Files: 35 (25 code + 8 doc + 2 script)
- Narrow-lens agents: 35 × 9 = 315
- Broad-lens agents: 6
- Consolidation agent: 1
- Triage agent: 1
- **Total: 323 agents**

## Batching Strategy

Max 16 concurrent agents. Narrow-lens reviews will be launched in batches of 16, waiting for each batch to complete before launching the next. This yields ~20 batches for narrow reviews. Broad-lens reviews run as a single batch of 6.
