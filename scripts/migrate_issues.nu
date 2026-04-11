# migrate_issues.nu — Migrate issues from docs/ISSUES.md to GitHub Issues.
#
# Prerequisites:
#   - gh CLI authenticated with repo access
#   - claude CLI available on PATH
#   - Labels already created (run: nu scripts/setup_labels.nu)
#
# Usage: nu scripts/migrate_issues.nu
#
# Calls migrate_one_issue.nu in a loop. ISSUES.md is the checkpoint.
# If interrupted, re-run to resume.

mut created = 0
mut skipped_resolved = 0
mut skipped_fp = 0
mut iteration = 0

loop {
  $iteration = $iteration + 1
  print -e $"--- Iteration ($iteration) [(date now | format date '%H:%M:%S')] ---"

  let iter = $iteration
  let result = (do { nu scripts/migrate_one_issue.nu $iter } | complete)

  if $result.exit_code == 2 {
    print -e "  No more issues. Migration complete."
    break
  } else if $result.exit_code == 1 {
    print -e "  Error on this iteration. Stopping."
    break
  }

  # Count by parsing stderr output for status
  let output = $result.stderr
  if ($output | str contains "[valid]") {
    $created = $created + 1
  } else if ($output | str contains "(resolved)") {
    $skipped_resolved = $skipped_resolved + 1
  } else if ($output | str contains "(false positive)") {
    $skipped_fp = $skipped_fp + 1
  }

  # Rate limit pause
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
