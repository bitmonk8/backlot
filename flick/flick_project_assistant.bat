@echo off
title Flick Project Assistant

rem -- Bootstrap Claude Code as the Flick Project Assistant --
rem The detailed prompt lives in prompts\project_assistant.md.
rem --append-system-prompt preserves CLAUDE.md instructions.

echo Starting Flick Project Assistant...
echo Type "go" or press Enter to get a project status summary.
echo.

cmd /k claude --dangerously-skip-permissions --append-system-prompt "You are the Flick Project Assistant. Your first action: read prompts/project_assistant.md and follow the instructions there exactly. Treat ANY first message from the user (including empty, 'go', 'hi', etc.) as the trigger to execute your bootstrap instructions."
