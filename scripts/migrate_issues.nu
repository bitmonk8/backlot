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
#   1. Claude extracts + validates + enriches the last issue → writes JSON to file
#   2. If valid, creates a GitHub issue via gh CLI
#   3. Claude removes the processed issue from ISSUES.md
#
# ISSUES.md itself is the checkpoint. If interrupted, re-run to resume.

let extract_template = (open scripts/extract_prompt.md)
let remove_template = (open scripts/remove_prompt.md)
let result_file = "scripts/.extract_result.json"

mut created = 0
mut skipped_resolved = 0
mut skipped_fp = 0
mut iteration = 0

loop {
  $iteration = $iteration + 1
  let iter_start = (date now)
  print -e $"--- Iteration ($iteration) [($iter_start | format date '%H:%M:%S')] ---"

  # Compute line count and inject into prompts
  let line_count = (open docs/ISSUES.md | lines | length)
  let tail_offset = [($line_count - 100) 0] | math max
  let extract_prompt = ($extract_template | str replace "{{LINE_COUNT}}" $"($line_count)" | str replace "{{TAIL_OFFSET}}" $"($tail_offset)")
  let remove_prompt = ($remove_template | str replace "{{LINE_COUNT}}" $"($line_count)" | str replace "{{TAIL_OFFSET}}" $"($tail_offset)")

  # Clean up previous result
  rm -f $result_file

  # Step 1: Extract + validate + enrich (writes JSON to file)
  print -e $"  Extracting last issue... \(($line_count) lines, offset ($tail_offset)\)"
  (^claude -p $extract_prompt --max-turns 50 --model claude-opus-4-6 --tools "Read,Grep,Glob,Write" --allowedTools "Read,Grep,Glob,Write" --no-session-persistence)

  # Read result from file
  if not ($result_file | path exists) {
    print -e "  ERROR: Claude did not write result file. Stopping."
    break
  }

  let content = (open $result_file)
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
    let result = (^gh issue create --title $title --body $body ...$label_args)
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
  (^claude -p $remove_prompt --max-turns 10 --model claude-sonnet-4-6 --tools "Read,Edit" --allowedTools "Read,Edit" --no-session-persistence)

  let iter_elapsed = ((date now) - $iter_start)
  print -e $"  Done in ($iter_elapsed | format duration sec). Sleeping 10s..."

  # Step 4: Rate limit pause
  sleep 10sec
}

# Cleanup
rm -f $result_file

# Summary
print ""
print $"Migration complete."
print $"  Created:          ($created)"
print $"  Skipped resolved: ($skipped_resolved)"
print $"  Skipped false positive: ($skipped_fp)"
print $"  Total processed:  ($created + $skipped_resolved + $skipped_fp)"
