# test_remove.nu — Test the remove prompt against the last issue in ISSUES.md.
# Usage: nu scripts/test_remove.nu
#
# After running, inspect with: git diff docs/ISSUES.md
# Restore with: git checkout docs/ISSUES.md

let line_count = (open docs/ISSUES.md | lines | length)
let tail_offset = [($line_count - 100) 0] | math max
let prompt = (open scripts/remove_prompt.md | str replace "{{LINE_COUNT}}" $"($line_count)" | str replace "{{TAIL_OFFSET}}" $"($tail_offset)")

print -e $"File: ($line_count) lines, reading from offset ($tail_offset)"
print -e "Running remove prompt..."

let raw = (
  ^claude -p $prompt
    --output-format json
    --max-turns 10
    --model claude-opus-4-6
    --tools "Read,Edit"
    --allowedTools "Read,Edit"
    --no-session-persistence
)

let response = ($raw | from json)

print -e $"Duration: ($response.duration_ms)ms, Cost: $($response.total_cost_usd), Turns: ($response.num_turns)"

let new_line_count = (open docs/ISSUES.md | lines | length)
print -e $"Lines before: ($line_count), after: ($new_line_count), removed: ($line_count - $new_line_count)"
print -e ""
print -e "Inspect: git diff docs/ISSUES.md"
print -e "Restore: git checkout docs/ISSUES.md"
