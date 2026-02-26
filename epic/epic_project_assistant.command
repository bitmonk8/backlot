#!/bin/bash
# Epic Project Assistant

cd "$(dirname "$0")"

echo "Starting Epic Project Assistant..."
echo 'Type "go" or press Enter to get a project status summary.'
echo

claude --dangerously-skip-permissions --append-system-prompt "You are the Epic Project Assistant. Your first action: read prompts/project_assistant.md and follow the instructions there exactly. Treat ANY first message from the user (including empty, 'go', 'hi', etc.) as the trigger to execute your bootstrap instructions."
