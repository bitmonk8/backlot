#!/usr/bin/env nu

# Periodic progress monitor for fix_issues_loop.nu.
# Polls the session directory and git log every N minutes, posts a short
# progress summary to the same Slack channel as the fix loop.
#
# Stop with:  echo "" | save .stop-monitor
#
# Usage:
#   nu monitor_loop.nu                       # 10 min interval, default channel
#   nu monitor_loop.nu --interval-min 5      # 5 min interval
#   nu monitor_loop.nu --once                # send one update and exit

def main [
  --interval-min: int = 10           # Minutes between checks
  --channel: string = "C0ATQ1JMV6H"  # Slack channel ID
  --once                             # Run a single check then exit
  --session-dir: string = ".pi/sessions"
] {
  let stop_file = ".stop-monitor"
  mkdir $session_dir

  # Anchor on current git HEAD so the first message reports activity since startup.
  let initial_head = (git rev-parse HEAD | str trim)
  let started_at = (date now)
  mut last_head = $initial_head
  mut tick = 0

  print $"Monitor started. Interval: ($interval_min) min. Channel: ($channel). Anchor commit: ($initial_head | str substring 0..7)"

  loop {
    if ($stop_file | path exists) {
      rm $stop_file
      print $"Stop requested via ($stop_file) — exiting"
      break
    }

    $tick = $tick + 1
    let summary = (build-progress-summary $session_dir $last_head $started_at $tick)
    print $"--- Tick ($tick) ---"
    print $summary

    send-slack-message $channel $session_dir $summary

    # Update last_head AFTER the report so the next tick reports new commits.
    $last_head = (git rev-parse HEAD | str trim)

    if $once { break }

    # Sleep in 5-second chunks so the stop sentinel is checked promptly.
    let total_seconds = ($interval_min * 60)
    mut elapsed = 0
    loop {
      if $elapsed >= $total_seconds { break }
      if ($stop_file | path exists) { break }
      sleep 5sec
      $elapsed = $elapsed + 5
    }
  }
}

# Build a compact progress message based on git log + session-dir state.
def build-progress-summary [
  session_dir: string,
  last_head: string,
  started_at: datetime,
  tick: int,
]: nothing -> string {
  let now = (date now)
  let uptime_min = ((($now - $started_at) / 1min) | math floor)

  # New commits since last tick (these correspond to fixed issues).
  let new_commits = (
    git log $"($last_head)..HEAD" --pretty=format:"%h %s" --no-merges
    | lines
    | where { $in | is-not-empty }
  )
  let new_commit_count = ($new_commits | length)

  # Latest session file = currently active or most recent agent call.
  let sessions = (
    ls $"($session_dir)/*.jsonl"
    | sort-by modified --reverse
  )
  let active_status = if ($sessions | is-empty) {
    "no session files yet"
  } else {
    let latest = ($sessions | first)
    let age_sec = ((($now - $latest.modified) / 1sec) | math floor)
    let size_kb = (($latest.size | into int) / 1024 | math floor)
    let age_label = if $age_sec < 60 {
      $"($age_sec)s ago"
    } else if $age_sec < 3600 {
      $"(($age_sec / 60) | math floor)m ago"
    } else {
      $"(($age_sec / 3600) | math floor)h ago"
    }
    let liveness = if $age_sec < 300 {
      ":green_circle: active"
    } else if $age_sec < 1800 {
      ":yellow_circle: idle"
    } else {
      ":red_circle: stale"
    }
    $"($liveness), latest session ($size_kb) KB, last write ($age_label)"
  }

  let total_sessions = ($sessions | length)

  let commits_section = if $new_commit_count == 0 {
    "_No new commits since last check._"
  } else {
    let commit_lines = ($new_commits | each { |c| $"  • `($c)`" } | str join "\n")
    $"*($new_commit_count) new commit\(s\):*\n($commit_lines)"
  }

  $":bar_chart: *Fix-loop monitor tick #($tick)* \(uptime ($uptime_min)m\)\n*Status:* ($active_status)\n*Total sessions logged:* ($total_sessions)\n($commits_section)"
}

# Send a Slack message via Pi (MCP). Uses the same temp-file + @file pattern
# as fix_issues_loop.nu's slack-report to dodge Nu's batch-arg restriction.
def send-slack-message [channel: string, session_dir: string, message: string] {
  mkdir $session_dir
  let tmp = $"($session_dir)/.tmp-monitor-msg.md"
  $message | save -f $tmp
  let prompt = $"Use the slack_send_message tool \(NOT slack_send_message_draft\) to post immediately to channel ($channel). The message body is in the attached file. Send its contents verbatim — do not modify, summarize, paraphrase, or add any commentary. Preserve all newlines and markdown exactly as written. This is automated unattended reporting; the message has already been reviewed, so do not create a draft or wait for confirmation — just send."
  let result = (do { ^pi -p --session-dir $session_dir $"@($tmp)" $prompt } | complete)
  if $result.exit_code != 0 {
    print $"WARNING: Slack send failed \(exit ($result.exit_code)\): ($result.stderr | str trim | lines | last 5 | str join '; ')"
  }
}
