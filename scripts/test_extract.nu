# test_extract.nu — Test the extract prompt against the last issue in ISSUES.md.
# Usage: nu scripts/test_extract.nu

let line_count = (open docs/ISSUES.md | lines | length)
let tail_offset = [($line_count - 100) 0] | math max
let prompt = (open scripts/extract_prompt.md | str replace "{{LINE_COUNT}}" $"($line_count)" | str replace "{{TAIL_OFFSET}}" $"($tail_offset)")

# Clean up previous result
rm -f scripts/.extract_result.json

print -e $"File: ($line_count) lines, reading from offset ($tail_offset)"
print -e "Running extract prompt..."

let raw = (
  ^claude -p $prompt
    --output-format json
    --max-turns 50
    --model claude-opus-4-6
    --tools "Read,Grep,Glob,Write"
    --allowedTools "Read,Grep,Glob,Write"
    --no-session-persistence
)

let response = ($raw | from json)

print -e $"Duration: ($response.duration_ms)ms, Cost: $($response.total_cost_usd), Turns: ($response.num_turns)"

# Read result from file
if not ("scripts/.extract_result.json" | path exists) {
  print -e "ERROR: Claude did not write scripts/.extract_result.json"
  print -e $"Claude's text output: ($response.result)"
  exit 1
}

let content = (open scripts/.extract_result.json)

print -e $"Status: ($content.status)"
if $content.status == "valid" {
  print -e $"Title: ($content.title)"
  print -e $"Labels: ($content.labels | str join ', ')"
  print -e "Body:"
  print $content.body
} else if $content.status == "empty" {
  print -e "No issues found."
} else {
  print -e $"Title: ($content.title | default 'n/a')"
}
