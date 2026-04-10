# Spec: Migrate ISSUES.md to GitHub Issues

## Goal

Migrate all active issues from `docs/ISSUES.md` into GitHub Issues on `bitmonk8/backlot`, then delete the file. After migration, GitHub Issues is the single source of truth for issue tracking.

## Source Inventory

`docs/ISSUES.md` is a consolidated, triage-enriched issue tracker. Last triaged 2026-04-09.

| Crate | Active issues | Medium-impact | Resolved (excluded) |
|-------|--------------|---------------|---------------------|
| Flick | 15 | 1 | 0 |
| Lot   | 58 | 1 | 0 |
| Reel  | 27 | 1 | 2 |
| Vault | 52 | 3 | 3 |
| Epic  | 77 | 11 | 1 |
| Mech  | 44 | 3 | 0 |
| **Total** | **~273** | **20** | **6** |

Resolved issues (marked ~~strikethrough~~) are excluded from migration. Each remaining entry becomes one GitHub issue — full granularity, independently assignable and closable.

## Label Taxonomy

4 axes, 20 labels. Every GitHub issue gets exactly 4 labels (one per axis).

### Crate (6 labels)

| Label | Hex |
|-------|-----|
| `crate:flick` | `#1f77b4` |
| `crate:lot` | `#2ca02c` |
| `crate:reel` | `#17becf` |
| `crate:vault` | `#9467bd` |
| `crate:epic` | `#ff7f0e` |
| `crate:mech` | `#d62728` |

### Importance (3 labels)

| Label | Hex |
|-------|-----|
| `importance:low` | `#f9d0c4` |
| `importance:medium` | `#e99695` |
| `importance:high` | `#d73a4a` |

### Effort (3 labels)

| Label | Hex |
|-------|-----|
| `effort:low` | `#c2e0c6` |
| `effort:medium` | `#91ca55` |
| `effort:high` | `#0e8a16` |

### Type (8 labels)

| Label | Hex | Maps from ISSUES.md categories |
|-------|-----|-------------------------------|
| `type:bug` | `#d4c5f9` | Correctness, Error Handling |
| `type:testing` | `#bfd4f2` | Testing, Test coverage gaps |
| `type:security` | `#f9c513` | Security |
| `type:performance` | `#fbca04` | Performance |
| `type:complexity` | `#c5def5` | Simplification, Separation of concerns, Duplication |
| `type:naming` | `#e6e6e6` | Naming |
| `type:docs` | `#d4c5f9` | Documentation, Cruft, Stale comments |
| `type:placement` | `#f0e68c` | Placement (wrong file/module) |

Labels provide the filterable scalar summary. The issue body carries full descriptive impact/fix cost paragraphs.

## GitHub Issue Body Format

Each created issue follows this structure:

```markdown
## File(s)

`path/to/file.rs` (lines X-Y)

## Issue

Description of the problem — what is wrong and why it matters.

## Impact

Descriptive paragraph(s) explaining specific consequences: correctness risk, maintenance burden,
developer confusion, etc. Not a scalar label — full sentences.

## Fix Cost

- **Risk:** Specific regression paths this fix could trigger.
- **Effort:** Concrete estimate — LOC, files touched, test cases needed.
- **Maintenance burden:** Net LOC change, new abstractions or invariants introduced.
```

Existing ISSUES.md group names (Lot Group 3, Vault T4, etc.) are noted in the body for context but do not affect issue structure.

## Migration Pipeline

A Nushell script orchestrates Claude Code CLI and `gh` CLI in a bottom-up consumption loop. Issues are processed from the end of ISSUES.md — the file itself is the progress checkpoint, shrinking each iteration.

### Architecture

```
Nushell script (orchestrator)
  │
  loop:
  │
  ├─ Step 1: EXTRACT + VALIDATE + ENRICH (read-only)
  │   claude -p $extract_prompt
  │     → reads last ~50 lines of ISSUES.md
  │     → if no issue found: returns {"status": "empty"} → exit loop
  │     → reads referenced source file(s) to validate
  │     → returns structured JSON
  │
  ├─ Step 2: CREATE GITHUB ISSUE (Nushell)
  │   if status == "valid":
  │     gh issue create --title $title --body $body --label $labels
  │   else if status == "false_positive" or "resolved":
  │     log skip reason
  │
  ├─ Step 3: REMOVE LAST ISSUE FROM FILE
  │   claude -p $remove_prompt
  │     → reads tail of ISSUES.md
  │     → edits file to remove the last issue entry
  │
  └─ Step 4: sleep ~10s (GitHub rate limit)
```

**Loop termination:** Invocation 1 returns `{"status": "empty"}` when it reads the tail of ISSUES.md and finds no issue entries remaining. Nushell exits the loop on this status. This costs one final Opus invocation to confirm completion.

No pre-splitting. No checkpoint file. If interrupted, re-run — remaining issues are still in the file.

**Edge case:** If Step 2 succeeds but Step 3 fails, the issue could be re-created on restart. Acceptable as a rare, manually fixable duplicate.

### Claude Code CLI Invocations

**Invocation 1 — Extract + Validate + Enrich (read-only):**
```
claude -p $extract_prompt --output-format json --max-turns 10 --model claude-opus-4-6 --allowedTools "Read,Grep,Glob"
```

Claude reads the tail of ISSUES.md, identifies the last issue, reads the referenced source files to confirm the issue still exists, enriches it with descriptive impact/fix cost paragraphs, assigns labels from the taxonomy, and returns structured JSON.

**Invocation 2 — Remove last issue from file:**
```
claude -p $remove_prompt --max-turns 3 --model claude-opus-4-6 --allowedTools "Read,Edit"
```

Claude reads the tail of ISSUES.md, identifies the last issue entry (from its heading through the `---` separator or end of file), and removes it. All other content and formatting is preserved.

### JSON Output Schema (Invocation 1)

```json
{
  "status": "valid | false_positive | resolved | empty",
  "title": "Short issue title",
  "labels": ["crate:flick", "importance:medium", "effort:low", "type:bug"],
  "body": "## File(s)\n\n..."
}
```

| Status | Action |
|--------|--------|
| `valid` | Create GitHub issue via `gh issue create` |
| `false_positive` | Skip issue, log reason, proceed to Step 3 (remove) |
| `resolved` | Skip issue, log as already fixed, proceed to Step 3 (remove) |
| `empty` | Exit loop — no issues remain in file |

When `status` is `empty`, `title`, `labels`, and `body` are absent.

### Runtime Estimate

~2-3 min per issue (two Opus invocations) + 10s sleep = ~10-14 hours for 273 issues. 546 total Claude invocations. Can run unattended.

## Deliverables

| File | Purpose |
|------|---------|
| `scripts/setup_labels.nu` | Creates project labels, removes GitHub defaults |
| `scripts/migrate_issues.nu` | Nushell orchestrator script |
| `scripts/extract_prompt.md` | Prompt template for extract+validate+enrich |
| `scripts/remove_prompt.md` | Prompt template for removing the last issue |

## Execution Phases

### Phase 1: Label Setup

Run `scripts/setup_labels.nu`. The script:

1. Deletes all default GitHub labels (`bug`, `documentation`, `duplicate`, `enhancement`, `good first issue`, `help wanted`, `invalid`, `question`, `wontfix`) to avoid confusion with the project's taxonomy.
2. Creates the 20 project labels defined in the Label Taxonomy section.

Idempotent — safe to re-run. Uses `gh label delete --yes` and `gh label create --force`.

### Phase 2: Run Migration Script

```
nu scripts/migrate_issues.nu
```

The script loops until `docs/ISSUES.md` has no issue headings remaining. Progress is visible as the file shrinks and stdout logs each result.

### Phase 3: Cleanup

1. Delete `docs/ISSUES.md` (empty or header-only after migration).
2. Update `docs/STATUS.md` — remove the `ISSUES.md` reference in the Lot section.
3. Optionally delete `scripts/migrate_issues.nu`, `scripts/extract_prompt.md`, `scripts/remove_prompt.md` (one-time use).
4. Commit.
