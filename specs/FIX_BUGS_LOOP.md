# Fix Issues Loop — Spec

## Overview

An automated issue-fixing pipeline for the Backlot monorepo. A Nushell script (`fix_issues_loop.nu`) drives Pi in non-interactive mode through a cycle of picking GitHub issues, fixing them, and pushing the results. A project-local Pi extension provides issue-querying commands. Slack messages report activity.

## Architecture

```
fix_issues_loop.nu          Nushell orchestrator (outer loop, error handling, Slack)
  ├─ /get-issues-summary    Pi extension command (query GitHub issues)
  ├─ /pick-issue            Pi extension command (select next issue)
  ├─ /implement             Personal Pi command (fix the issue, unmodified)
  ├─ /commit-push-check     Personal Pi command (commit and push, unmodified)
  └─ gh issue close         Direct CLI call after successful push
```

Each Pi invocation uses `pi -p --mode json --no-session` — print mode, JSON output, ephemeral session. This guarantees fresh context per step and structured output parseable by Nushell.

## Nushell Script: `fix_issues_loop.nu`

### CLI Flags

| Flag | Type | Description |
|------|------|-------------|
| `--crate` | `string` | Filter to a single crate (e.g., `mech`) |
| `--importance` | `string` | Filter by importance label (`high`, `medium`, `low`) |
| `--effort` | `string` | Filter by effort label (`high`, `medium`, `low`) |
| `--type` | `string` | Filter by type label (`bug`, `docs`, `testing`, etc.) |
| `--once` | `bool` | Run a single iteration then exit |

Filters are passed through to `/get-issues-summary` and `/pick-issue` as label constraints on `gh issue list`.

### Loop

```
precondition: clean working tree, correct branch

loop {
    get issues summary (with filters)
    if no issues remain → break

    pick next issue (excluding previously failed issues)
    invoke /implement with issue number and title
    if failed → record failure, report to Slack, backoff if needed, continue

    invoke /commit-push-check
    if failed → record failure, report to Slack, continue

    gh issue close <N>
    report success to Slack
    reset consecutive failure counter
}

send completion summary to Slack
```

### Preconditions

The script verifies before starting:
- Working tree is clean (`git status --porcelain` is empty)
- On the expected branch

Aborts with a clear error message if either check fails.

### State Tracking

| Field | Type | Description |
|-------|------|-------------|
| `total_attempted` | `int` | Issues attempted this run |
| `total_succeeded` | `int` | Issues fixed and pushed |
| `total_failed` | `int` | Issues that failed |
| `consecutive_failures` | `int` | Reset to 0 on success |
| `issues_skipped` | `list<int>` | Issue numbers that failed (excluded from `/pick-issue`) |

### Error Handling

| Failure | Behavior |
|---------|----------|
| Pi exits non-zero | Log, report to Slack, skip issue |
| `/implement` fails | Report to Slack, add to skip list, continue |
| `/commit-push-check` fails | Report to Slack, continue |
| N consecutive failures | Exponential backoff, then continue |
| GitHub API unreachable | Retry with backoff, abort after 5 retries |
| All issues exhausted | Break loop, send summary |

### Backoff

```
sleep_seconds = min(30 * 2^(consecutive_failures - 1), 600)
```

Applied after each failure before the next iteration.

### Slack Reporting

Uses Pi with Slack MCP tool:

```
pi -p --no-session "Send a Slack message to channel <CHANNEL_ID>: <message>"
```

**Events reported:**

| Event | Message |
|-------|---------|
| Loop started | "Fix-issues loop started. {N} open issues." |
| Issue picked | "Working on #{number}: {title}" |
| Issue fixed | ":white_check_mark: Fixed #{number}: {title}" |
| Issue failed | ":x: Failed #{number}: {title} — {reason}" |
| Backoff | ":hourglass: {N} consecutive failures, backing off {sleep}s" |
| Loop complete | "Fix-issues loop complete. {succeeded}/{attempted} fixed, {remaining} remaining." |

Channel: `#thomasa-agent-activity` (`C0ATQ1JMV6H`).

## Pi Extension: `.pi/extensions/fix-issues-loop/`

Project-local extension providing two commands.

### `/get-issues-summary`

Runs `gh issue list --repo bitmonk8/backlot --state open --json number,title,labels,state --limit 100`.

Parses labels into structured fields using the `crate:*`, `importance:*`, `effort:*`, `type:*` conventions.

Output:
```json
{
  "count": 30,
  "issues": [
    {
      "number": 463,
      "title": "mech: run_function_imperative commits side effects...",
      "crate": "mech",
      "importance": "medium",
      "effort": "medium",
      "type": "bug"
    }
  ]
}
```

### `/pick-issue`

Accepts optional args: crate filter, comma-separated issue numbers to skip.

Prioritization:
1. `importance:high` > `medium` > `low`
2. `effort:low` > `medium` > `high`
3. `type:bug` first, then other types

Output:
```json
{
  "number": 463,
  "title": "mech: run_function_imperative commits side effects...",
  "crate": "mech",
  "importance": "medium",
  "effort": "medium",
  "type": "bug",
  "url": "https://github.com/bitmonk8/backlot/issues/463"
}
```

Returns `{ "number": null }` when no issues remain after filtering.

## Structured Output

Extension commands (`/get-issues-summary`, `/pick-issue`) are pure data commands — no LLM involved. The command handler runs `gh issue list`, processes the results, and prints JSON to stdout via `console.log()`. Pi exits immediately after the command handler returns.

The Nushell script captures stdout and parses the last JSON line:

```nushell
let result = (do { ^pi -p --no-session "/get-issues-summary" } | complete)
let parsed = ($result.stdout | lines | where { $in | is-not-empty } | last | from json)
```

Agent commands (`/implement`, `/commit-push-check`) use Pi's normal agent loop. The script checks only the exit code — zero means success.

## File Layout

```
backlot/
├── fix_issues_loop.nu
└── .pi/
    └── extensions/
        └── fix-issues-loop/
            ├── index.ts
            └── commands/
                ├── get-issues-summary.ts
                └── pick-issue.ts
```

## Configuration

| Variable | Purpose | Default |
|----------|---------|---------|
| `SLACK_CHANNEL_ID` | Slack channel for reports | `C0ATQ1JMV6H` |
| `FIX_LOOP_MAX_ISSUES` | Cap total issues per run | unlimited |
| `FIX_LOOP_MODEL` | Pi model override | project default |
| `FIX_LOOP_DRY_RUN` | Skip commit/push, just report | false |

## Future Work

- **Worktrees:** Per-issue worktrees in `.trees/` for isolated fixes, enabling parallel lanes.
- **Branch strategy:** Per-issue branches with PRs and auto-merge instead of pushing to main.
