# Flick — Status

## Current State

Monadic architecture implemented. Flick makes a single model call per invocation and returns a JSON result. The caller drives the agent loop. Two-step structured output for Chat Completions providers (tools + output_schema) implemented in the runner.

## Next Work

- reqwest 0.13 upgrade (blocked by rustc ICE on `windows-sys` 0.61.2)
- Fix Later items (see `BACKLOG.md`)
