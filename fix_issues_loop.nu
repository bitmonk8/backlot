#!/usr/bin/env nu

# Automated issue-fixing loop for the Backlot monorepo.
# Drives Pi in non-interactive mode to pick, fix, and push GitHub issue fixes.

def main [
  --crate: string        # Filter to a single crate (e.g., mech)
  --importance: string   # Filter by importance label (high, medium, low)
  --effort: string       # Filter by effort label (high, medium, low)
  --type: string         # Filter by type label (bug, docs, testing, etc.)
  --once                 # Run a single iteration then exit
  --max-issues: int      # Cap total issues per run
  --dry-run              # Skip commit/push, just report
  --model: string = "claude-opus-4-7"  # Pi model
  --branch: string = "main"  # Expected git branch
] {
  let slack_channel = ($env.SLACK_CHANNEL_ID? | default "C0ATQ1JMV6H")

  # --- Preconditions ---
  let dirty = (git status --porcelain | str trim)
  if ($dirty | is-not-empty) {
    print $"ERROR: Working tree is not clean:\n($dirty)"
    exit 1
  }

  let current_branch = (git branch --show-current | str trim)
  if $current_branch != $branch {
    print $"ERROR: Expected branch '($branch)', currently on '($current_branch)'"
    exit 1
  }

  # --- Build filter args for extension commands ---
  mut filter_args = ""
  if $crate != null { $filter_args = $"($filter_args) --crate=($crate)" }
  if $importance != null { $filter_args = $"($filter_args) --importance=($importance)" }
  if $effort != null { $filter_args = $"($filter_args) --effort=($effort)" }
  if $type != null { $filter_args = $"($filter_args) --type=($type)" }
  $filter_args = ($filter_args | str trim)

  # --- Build pi args for agent commands (implement, commit-push-check) ---
  mut pi_agent_args = ["-p" "--no-session"]
  if $model != null { $pi_agent_args = ($pi_agent_args | append ["--model" $model]) }

  # --- State ---
  mut total_attempted = 0
  mut total_succeeded = 0
  mut total_failed = 0
  mut consecutive_failures = 0
  mut issues_skipped: list<int> = []

  # --- Report initial issue count ---
  let summary_cmd = if ($filter_args | is-empty) { "/get-issues-summary" } else { $"/get-issues-summary ($filter_args)" }
  let summary = (run-pi-data-command $summary_cmd)
  if $summary == null {
    print "ERROR: Failed to get issues summary"
    exit 1
  }
  let initial_count = ($summary | get count)
  print $"Found ($initial_count) open issues"
  slack-report $pi_agent_args $slack_channel $"Fix-issues loop started. ($initial_count) open issues."

  # --- Main loop ---
  loop {
    # Check max issues cap
    if $max_issues != null and $total_attempted >= $max_issues {
      print $"Reached max issues cap \(($max_issues)\)"
      break
    }

    # Build skip arg
    let skip_arg = if ($issues_skipped | is-empty) { "" } else {
      $"--skip=($issues_skipped | each { into string } | str join ',')"
    }
    let pick_args = ([$filter_args $skip_arg] | where { $in | is-not-empty } | str join " ")

    # Pick next issue
    let pick_cmd = if ($pick_args | is-empty) { "/pick-issue" } else { $"/pick-issue ($pick_args)" }
    let picked = (run-pi-data-command $pick_cmd)
    if $picked == null or ($picked | get number) == null {
      print "No more issues to work on"
      break
    }

    let issue_num = ($picked | get number)
    let issue_title = ($picked | get title)
    print $"--- Attempting issue #($issue_num): ($issue_title) ---"
    slack-report $pi_agent_args $slack_channel $"Working on #($issue_num): ($issue_title)"
    $total_attempted = $total_attempted + 1

    # --- Implement ---
    let implement_result = (run-pi-agent $pi_agent_args $"/implement Fix issue #($issue_num): ($issue_title)")
    if $implement_result != 0 {
      print $"FAILED: /implement exited with code ($implement_result)"
      $total_failed = $total_failed + 1
      $consecutive_failures = $consecutive_failures + 1
      $issues_skipped = ($issues_skipped | append $issue_num)
      slack-report $pi_agent_args $slack_channel $":x: Failed #($issue_num): ($issue_title) — implement failed"
      do-backoff $consecutive_failures
      if $once { break }
      continue
    }

    # --- Commit and push ---
    if $dry_run {
      print $"DRY RUN: Skipping commit/push for #($issue_num)"
      git checkout -- . | ignore
      git clean -fd | ignore
    } else {
      let commit_result = (run-pi-agent $pi_agent_args "/commit-push-check")
      if $commit_result != 0 {
        print $"FAILED: /commit-push-check exited with code ($commit_result)"
        $total_failed = $total_failed + 1
        $consecutive_failures = $consecutive_failures + 1
        $issues_skipped = ($issues_skipped | append $issue_num)
        slack-report $pi_agent_args $slack_channel $":x: Failed #($issue_num): ($issue_title) — commit/push failed"
        if $once { break }
        continue
      }

      # Close the issue
      let close_result = (do { gh issue close $issue_num --repo bitmonk8/backlot } | complete)
      if $close_result.exit_code != 0 {
        print $"WARNING: Failed to close issue #($issue_num): ($close_result.stderr)"
      }
    }

    print $"SUCCESS: Fixed issue #($issue_num)"
    $total_succeeded = $total_succeeded + 1
    $consecutive_failures = 0
    slack-report $pi_agent_args $slack_channel $":white_check_mark: Fixed #($issue_num): ($issue_title)"

    if $once { break }
  }

  # --- Summary ---
  let remaining = $initial_count - $total_succeeded
  let summary_msg = $"Fix-issues loop complete. ($total_succeeded)/($total_attempted) fixed, ($remaining) remaining."
  print $summary_msg
  slack-report $pi_agent_args $slack_channel $summary_msg
}

# Run a Pi extension command that prints JSON. No LLM involved.
# Pi print mode routes extension console.log to stderr, so check both streams.
def run-pi-data-command [command: string]: nothing -> any {
  let result = (do { ^pi -p --no-session $command } | complete)
  if $result.exit_code != 0 {
    print $"ERROR: pi command '($command)' failed \(exit ($result.exit_code)\)"
    return null
  }

  # Extension output may land on stdout or stderr depending on Pi mode.
  # Search both streams for the last JSON line.
  let all_lines = ([$result.stdout $result.stderr] | str join "\n" | lines | where { $in | is-not-empty })
  mut parsed = null
  for $line in ($all_lines | reverse) {
    try {
      $parsed = ($line | from json)
      break
    } catch {
      continue
    }
  }

  if $parsed == null {
    print $"ERROR: No JSON output from '($command)'"
  }
  $parsed
}

# Run a Pi agent command (like /implement). Returns exit code.
def run-pi-agent [pi_args: list<string>, command: string]: nothing -> int {
  let result = (do { ^pi ...$pi_args $command } | complete)
  $result.exit_code
}

# Send a Slack message via Pi with MCP Slack tool.
def slack-report [pi_args: list<string>, channel: string, message: string] {
  let prompt = $"Send a Slack message to channel ($channel) with this exact text: ($message)"
  let result = (do { ^pi -p --no-session $prompt } | complete)
  if $result.exit_code != 0 {
    print $"WARNING: Slack report failed: ($result.stderr)"
  }
}

# Exponential backoff: min(30 * 2^(n-1), 600) seconds.
def do-backoff [consecutive: int] {
  if $consecutive <= 0 { return }
  let seconds = [((30 * (2 ** ($consecutive - 1))) | into int) 600] | math min
  print $":hourglass: ($consecutive) consecutive failures, backing off ($seconds)s"
  sleep ($seconds * 1sec)
}
