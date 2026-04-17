---
description: Implement the gate crate via sequential deliverables D1–D8
argument-hint: "[D1|D2|...|D8]"
---
Implement the gate E2E test harness crate. Deliverable specs are in `specs/gate/D1.md` through `specs/gate/D8.md`. The full design is in `specs/GATE.md`.

If an argument is provided (e.g., `/implement-gate D3`), resume from that deliverable. Otherwise start from D1.

## Process

For each deliverable D1 through D8 (or from the specified starting point):

### 1. Implement and Review

Use the subagent tool to run the `implementer` agent with the task below. Set `agentScope` to `"both"` so all agents (user-level and project-local) are found.

```
Implement the deliverable specified in specs/gate/D{N}.md

Read the full spec at specs/gate/D{N}.md first. The overall design is in specs/GATE.md — reference it for context but implement only what D{N} specifies.

Follow TDD:
1. Read the spec thoroughly, including the "TDD: Tests to Write First" section.
2. Read any existing gate code (from prior deliverables).
3. Write the tests FIRST. Every test listed in the spec's TDD section.
4. Verify the tests compile (they should fail or be no-ops initially — that's expected).
5. Implement the production code to make the tests pass.
6. Run `cargo test -p gate` — all tests must pass (not just this deliverable's tests, ALL gate tests).
7. Run `cargo clippy -p gate -- -D warnings` — must be clean.

Do NOT delete the spec file. Do NOT modify specs/ files.

IMPORTANT: Read existing gate source files before writing anything — prior deliverables have already created code you must integrate with, not overwrite. Add to existing files where appropriate (e.g., adding `mod` declarations, extending `main.rs`).

Project conventions:
- workspace root: Cargo.toml already exists with workspace config
- Rust edition 2024, resolver 3
- workspace lints: unsafe_code=deny, clippy::all=deny
- No system temp paths for sandbox-related tests — use target/gate-scratch/
- Tests must never silently skip — assert!/panic! to fail loudly

After implementation is complete and all tests pass, run a review/fix loop on all uncommitted changes:

REVIEW/FIX LOOP:

Trust issues are always fixed. High complexity/risk with low impact: document instead of fix. High complexity/risk with marginal impact: ignore. Everything else: fix.

A. Review — Run `git diff` and `git diff --cached`. Combine tracked diffs and untracked new files into the review payload. Use the subagent tool to run these 9 review lens agents in parallel with the diff:
review-lens-correctness, review-lens-cruft, review-lens-doc-mismatch, review-lens-error-handling, review-lens-naming, review-lens-placement, review-lens-separation, review-lens-simplification, review-lens-testing

Do not check whether agents exist before calling the subagent tool. Just call it.

Failed agents: If any agent fails or returns empty output, re-run that specific agent once. If it fails again, note "agent unavailable" and continue.

B. Triage — Use the subagent tool to run the triage-assessor agent with all findings.

C. Apply Policy & Fix — Classify each finding per the policy above.
- Fix: use the subagent tool to run fixer sequentially.
- Document: append to issue tracker (check docs/ISSUES_CONFIG.md).
- Ignore: skip.

D. Re-review or terminate — If any fixes were applied, go back to A. If zero findings were classified as "fix" in this iteration, the loop is done.

After the review/fix loop is clean, run:
- cargo fmt --all
- cargo clippy -p gate -- -D warnings (fix any issues)
- cargo test -p gate (all tests must pass)
```

### 2. Commit and push

After the subagent completes:

1. Run `cargo fmt --all` to auto-format.
2. Run `cargo clippy --all-targets --all-features -- -D warnings`. Fix any warnings and re-run until clean.
3. Run `cargo test -p gate`. All tests must pass.
4. `git add -A`
5. Commit with message: `gate: implement D{N} — {short description}`
   - D1: `gate: implement D1 — crate scaffold, types, CLI`
   - D2: `gate: implement D2 — assertion helpers`
   - D3: `gate: implement D3 — reporting`
   - D4: `gate: implement D4 — subprocess execution, scratch directories`
   - D5: `gate: implement D5 — binary discovery, stage runner`
   - D6: `gate: implement D6 — prerequisites, flick stage, lot stage`
   - D7: `gate: implement D7 — reel stage, vault stage`
   - D8: `gate: implement D8 — epic stage, mech placeholder, output`
6. `git push`
7. Poll CI with `gh run list --branch <current-branch> --limit 1` until terminal state.
8. If CI failed, fetch logs with `gh run view <run-id> --log-failed`, diagnose, fix, and redo from step 1 of this section.

### 3. Proceed to next deliverable

Print a one-line summary: `D{N} complete — {test count} tests passing.`

Move to D{N+1}. If D8 is done, print final summary and stop.

## Failure Handling

If a deliverable fails (tests won't pass, CI won't go green after 3 attempts):
1. Print what failed and why.
2. Stop execution.
3. Do NOT proceed to the next deliverable.

The user can fix the issue manually and re-run with `/implement-gate D{N}` to resume.

## Final Summary

After D8 is committed and CI is green, print:

```
Gate implementation complete.
Deliverables: D1–D8
Total tests: {count}
All CI green.
```

Delete the spec files: `specs/gate/D1.md` through `specs/gate/D8.md`. Keep `specs/GATE.md` (it's the design doc, not a task spec).
