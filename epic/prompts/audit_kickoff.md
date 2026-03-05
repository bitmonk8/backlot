# Audit Kickoff Prompt

Use this prompt in a fresh Claude Code session (with Opus) to execute the full project audit.

---

## Prompt

You are executing a full project audit for the Epic codebase. Your job is to launch review agents, monitor completion, and consolidate results.

**Important: You must use Opus for all subagents. Since the session is already running Opus, all Agent tool calls will use Opus automatically.**

### Step 1: Read the audit plan

Read `docs/AUDIT.md` to understand the full review matrix, code units, review types, cross-cutting reviews, and broad-lens reviews.

### Step 2: Create the output directory

Create the directory `docs/audit/` for agent output files.

### Step 3: Launch all matrix cell agents (81 tasks)

For every active cell in the Review Matrix (cells with `[ ]`, not `--`), launch a background agent. Each agent:

- **Reads only the files for its code unit** (listed in the Code Units table).
- **Reviews through the lens of its review type only** (defined in the Review Types table).
- **For R7 (Design intent):** Also read `docs/TASK_MODEL.md`, `docs/AGENT_DESIGN.md`, `docs/VERIFICATION.md`, `docs/FIX_LOOP_SPEC.md`, and `docs/ARCHITECTURE.md` as relevant context for the unit under review.
- **For R8 (Doc consistency):** Read every doc in `docs/` and compare against the source files they describe.
- **For R4 (Dead code & cruft):** Use Grep to search for: `TODO`, `FIXME`, `HACK`, `XXX`, `dead_code`, `allow(`, `zeroclaw`, `zclaw`, `serde_yaml`, `flick_path`, `work_dir`, `subprocess`, `spawn`, `child process` within the unit's files.
- **Writes its findings to `docs/audit/{cell_id}.md`** where `cell_id` is the cell identifier (e.g., `U1-R1.md`, `U5-R2.md`).

The output file format for each agent:

```markdown
# {cell_id}: {unit name} / {review type name}

## Summary

{1-2 sentence summary of what was reviewed and overall assessment}

## Findings

| # | Severity | Finding | Location | Suggestion |
|---|----------|---------|----------|------------|
| 1 | major | {description} | `file.rs:123` | {fix suggestion} |
| 2 | minor | {description} | `file.rs:456` | {fix suggestion or "—"} |

## Notes

{Any context, caveats, or observations that don't fit the findings table}
```

Severity levels: `critical` (correctness/security bug), `major` (significant issue), `minor` (improvement opportunity), `note` (observation, no action needed).

If the agent finds no issues, it should still write the file with an empty findings table and a note saying "No issues found."

### Step 4: Launch cross-cutting agents (6 tasks)

Launch these in parallel with the matrix agents:

- **X1** (`docs/audit/X1.md`): Read `Cargo.toml` and `Cargo.lock`. Check for unused dependencies, feature flags, edition, metadata.
- **X2** (`docs/audit/X2.md`): Run `cargo clippy -- -W clippy::pedantic 2>&1` and triage the output. Categorize warnings by severity.
- **X3** (`docs/audit/X3.md`): Run `cargo build 2>&1` and check for warnings. Also grep for `#[allow(` across all `.rs` files to find suppressed warnings.
- **X4** (`docs/audit/X4.md`): Assess CI readiness. Read the project structure, Cargo.toml, test setup. Describe what a CI pipeline would need.
- **X5** (`docs/audit/X5.md`): Read all `.rs` files (just the pub items and module structure, not full contents). Review naming consistency, visibility boundaries, module organization.
- **X6** (`docs/audit/X6.md`): Grep for all hardcoded constants (`const `) across the codebase. For each, assess whether it should be configurable via `epic.toml`.

Same output format as matrix cells.

### Step 5: Launch broad-lens agents (8 tasks)

**These should run after the matrix and cross-cutting agents complete**, because they must not duplicate matrix findings. However, since agents write to separate files, you can launch them in parallel with the others — the consolidation step will handle deduplication notes.

Each broad-lens agent:

- **Reads all files listed in its scope** (see Broad-Lens Reviews section of AUDIT.md).
- **Must NOT raise issues identifiable within a single code unit.** Only flag issues that require examining 2+ modules together to detect.
- **Writes to `docs/audit/{cell_id}.md`** (e.g., `B1.md`, `B5.md`).

Same output format as matrix cells.

### Step 6: Wait for all agents to complete

Monitor all background agents. Do not proceed to consolidation until every agent has finished and written its output file.

### Step 7: Consolidate findings into AUDIT.md

Once all 95 agents have completed:

1. Read every file in `docs/audit/`.
2. For each matrix cell, update the checkbox in the Review Matrix from `[ ]` to `[x]`.
3. For each cross-cutting and broad-lens review, update its checkbox from `[ ]` to `[x]`.
4. In the Findings section of AUDIT.md, create one subsection per cell that has findings (skip cells with no issues). Use the format already specified in AUDIT.md.
5. Update the Summary table: fill in the "Done" and "Issues Found" columns with actual counts.
6. Add a new section at the end: `## Audit Results Summary` with:
   - Total findings by severity (critical/major/minor/note).
   - Top 5 most concerning findings across all cells.
   - Recommended action items in priority order.

### Parallelism guidance

- Launch agents in batches to avoid overwhelming the system. Suggested: 15-20 agents per batch.
- All matrix cells, cross-cutting, and broad-lens agents are independent of each other — maximize parallelism.
- The consolidation step (Step 7) depends on all agents completing.

### File mapping reference

For quick reference, here are the source files for each code unit:

| Unit | Files (all paths relative to project root) |
|------|---------------------------------------------|
| U1 | `src/orchestrator.rs` |
| U2 | `src/agent/flick.rs` |
| U3 | `src/agent/config_gen.rs` |
| U4 | `src/agent/prompts.rs` |
| U5 | `src/agent/tools.rs` |
| U6 | `src/agent/models.rs`, `src/agent/mod.rs` |
| U7 | `src/task/mod.rs`, `src/task/assess.rs`, `src/task/leaf.rs`, `src/task/branch.rs`, `src/task/verify.rs` |
| U8 | `src/state.rs` |
| U9 | `src/events.rs` |
| U10 | `src/config/mod.rs`, `src/config/project.rs` |
| U11 | `src/init.rs` |
| U12 | `src/cli.rs`, `src/main.rs` |
| U13 | `src/tui/mod.rs`, `src/tui/task_tree.rs`, `src/tui/worklog.rs`, `src/tui/metrics.rs` |
| U14 | `src/git.rs` |
| U15 | `src/metrics.rs` |
| U16 | `src/services/mod.rs`, `src/services/document_store.rs`, `src/services/research.rs`, `src/services/verification.rs` |
| U17 | All files in `docs/` |
