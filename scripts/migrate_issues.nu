# migrate_issues.nu — Migrate issues from docs/ISSUES.md to GitHub Issues.
#
# Prerequisites:
#   - gh CLI authenticated with repo access
#   - claude CLI available on PATH
#   - Labels already created (run: nu scripts/setup_labels.nu)
#
# Usage: nu scripts/migrate_issues.nu
#
# The script consumes docs/ISSUES.md from the bottom up. Each iteration:
#   1. Claude extracts + validates + enriches the last issue (read-only)
#   2. If valid, creates a GitHub issue via gh CLI
#   3. Claude removes the processed issue from the file
#
# ISSUES.md itself is the checkpoint. If interrupted, re-run to resume.

let extract_prompt = (open scripts/extract_prompt.md)
let remove_prompt = (open scripts/remove_prompt.md)

mut created = 0
mut skipped_resolved = 0
mut skipped_fp = 0
mut iteration = 0

loop {
  $iteration = $iteration + 1
  print -e $"--- Iteration ($iteration) ---"

  # Step 1: Extract + validate + enrich (read-only)
  print -e "  Extracting last issue..."
  let raw = (
    ^claude -p $extract_prompt
      --output-format json
      --max-turns 10
      --model claude-opus-4-6
      --tools "Read,Grep,Glob"
      --allowedTools "Read,Grep,Glob"
      --bare
      --no-session-persistence
  )

  let response = ($raw | from json)
  let content_text = ($response.result | default "")

  # Try to parse Claude's text output as JSON
  let content = try {
    $content_text | from json
  } catch {
    print -e $"  ERROR: Failed to parse Claude output as JSON. Raw output:"
    print -e $content_text
    print -e "  Stopping."
    break
  }

  let status = ($content.status | default "unknown")

  # Check for loop termination
  if $status == "empty" {
    print -e "  No more issues found. Migration complete."
    break
  }

  let title = ($content.title | default "<untitled>")
  print -e $"  Issue: ($title) [($status)]"

  # Step 2: Create GitHub issue if valid
  if $status == "valid" {
    let labels = ($content.labels | default [])
    let body = ($content.body | default "")

    print -e "  Creating GitHub issue..."
    let label_args = ($labels | each { |l| ["--label" $l] } | flatten)
    let result = (
      ^gh issue create
        --title $title
        --body $body
        ...$label_args
    )
    print -e $"  Created: ($result)"
    $created = $created + 1
  } else if $status == "resolved" {
    print -e "  Skipping (resolved)."
    $skipped_resolved = $skipped_resolved + 1
  } else if $status == "false_positive" {
    print -e "  Skipping (false positive)."
    $skipped_fp = $skipped_fp + 1
  } else {
    print -e $"  Unknown status: ($status). Stopping."
    break
  }

  # Step 3: Remove last issue from ISSUES.md
  print -e "  Removing issue from ISSUES.md..."
  ^claude -p $remove_prompt
    --max-turns 3
    --model claude-opus-4-6
    --tools "Read,Edit"
    --allowedTools "Read,Edit"
    --bare
    --no-session-persistence

  # Step 4: Rate limit pause
  print -e "  Sleeping 10s..."
  sleep 10sec
}

# Summary
print ""
print $"Migration complete."
print $"  Created:          ($created)"
print $"  Skipped resolved: ($skipped_resolved)"
print $"  Skipped false positive: ($skipped_fp)"
print $"  Total processed:  ($created + $skipped_resolved + $skipped_fp)"
