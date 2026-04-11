# migrate_one_issue.nu — Process one issue from docs/ISSUES.md.
#
# Usage:
#   nu scripts/migrate_one_issue.nu                    # normal mode
#   nu scripts/migrate_one_issue.nu 64                 # with iteration number
#   nu scripts/migrate_one_issue.nu 64 --debug         # debug mode (sessions persisted)
#
# Exit codes:
#   0 — issue processed (created, resolved, or false_positive)
#   1 — error
#   2 — no more issues (empty)

def main [
  iteration: int = 0        # iteration number (for display only)
  --debug                   # keep Claude sessions for debugging
] {
  let iter_start = (date now)
  let result_file = "scripts/.extract_result.json"
  let extract_template = (open scripts/extract_prompt.md)
  let remove_template = (open scripts/remove_prompt.md)

  if $debug { print -e "  [debug] Debug mode ON — Claude sessions will be persisted" }

  # Compute line count and inject into prompts
  let line_count = (open docs/ISSUES.md | lines | length)
  let tail_offset = [($line_count - 100) 0] | math max
  let extract_prompt = ($extract_template | str replace "{{LINE_COUNT}}" $"($line_count)" | str replace "{{TAIL_OFFSET}}" $"($tail_offset)")
  let remove_prompt = ($remove_template | str replace "{{LINE_COUNT}}" $"($line_count)" | str replace "{{TAIL_OFFSET}}" $"($tail_offset)")

  if $debug { print -e $"  [debug] ISSUES.md: ($line_count) lines, tail offset: ($tail_offset)" }

  # Clean up previous result
  rm -f $result_file

  # Step 1: Extract + validate + enrich
  print -e $"  Extracting last issue... \(($line_count) lines, offset ($tail_offset)\)"
  let extract_start = (date now)

  let session_flag = if $debug { [] } else { [--no-session-persistence] }
  (^claude -p $extract_prompt --max-turns 50 --model claude-opus-4-6 --tools "Read,Grep,Glob,Write" --allowedTools "Read,Grep,Glob,Write" ...$session_flag)

  let extract_elapsed = ((date now) - $extract_start)
  if $debug { print -e $"  [debug] Extract took ($extract_elapsed | format duration sec)" }

  # Read result from file
  if not ($result_file | path exists) {
    print -e "  ERROR: Claude did not write result file."
    exit 1
  }

  let content = (open $result_file)
  let status = ($content.status | default "unknown")

  if $debug { print -e $"  [debug] Result status: ($status)" }

  # Check for empty
  if $status == "empty" {
    print -e "  No more issues found."
    exit 2
  }

  let title = ($content.title | default "<untitled>")
  print -e $"  Issue: ($title) [($status)]"

  # Step 2: Create GitHub issue if valid
  if $status == "valid" {
    let labels = ($content.labels | default [])
    let body = ($content.body | default "")

    if $debug {
      print -e $"  [debug] Labels: ($labels | str join ', ')"
      print -e $"  [debug] Body length: ($body | str length) chars"
    }

    print -e "  Creating GitHub issue..."
    let label_args = ($labels | each { |l| ["--label" $l] } | flatten)
    let result = (^gh issue create --title $title --body $body ...$label_args)
    print -e $"  Created: ($result)"
  } else if $status == "resolved" {
    print -e "  Skipping (resolved)."
  } else if $status == "false_positive" {
    print -e "  Skipping (false positive)."
  } else {
    print -e $"  Unknown status: ($status)."
    exit 1
  }

  # Step 3: Remove last issue from ISSUES.md
  print -e "  Removing issue from ISSUES.md..."
  let remove_start = (date now)

  (^claude -p $remove_prompt --max-turns 10 --model claude-sonnet-4-6 --tools "Read,Edit" --allowedTools "Read,Edit" ...$session_flag)

  let remove_elapsed = ((date now) - $remove_start)
  if $debug { print -e $"  [debug] Remove took ($remove_elapsed | format duration sec)" }

  let iter_elapsed = ((date now) - $iter_start)
  print -e $"  Done in ($iter_elapsed | format duration sec). [($status)]"
}
