# Integration Test Failures: reel_read, reel_write, reel_edit

## Failing tests

- `integration_custom_command_reel_read` — `ls $full` returns "No matches found"
- `integration_custom_command_reel_write` — `mkdir $parent` returns "Already exists"
- `integration_custom_command_reel_edit` — `open $full --raw` returns `nothing`, `split row` fails on type mismatch

## Passing tests (same sandbox setup)

- `integration_custom_command_reel_glob` — uses `cd` + `glob` built-in
- `integration_custom_command_reel_grep` — shells out to `rg` via `REEL_RG_PATH`

## What has been ruled out

**Missing ACEs / ancestor traversal is not the cause.** Verified by:

1. Moving the test sandbox directory from a project sibling (`../reel-sandbox-test/`) to inside the project (`reel/target/sandbox-test/`) — same failures.
2. The sandbox spawns successfully and nu executes commands. 19 other sandbox tests pass in the same environment, including `reel glob` and `reel grep` which use the identical `sandbox_env()` setup.
3. `appcontainer_prerequisites_met` checks for ALL APPLICATION PACKAGES traverse ACEs, but AppContainer processes can traverse directories via other ACEs (per-user, Everyone). Tests under `%TEMP%` work without ALL APPLICATION PACKAGES ACEs on ancestors.

## What is known

The failures are specific to nu's `ls`, `open`, and `save` built-in commands when invoked on absolute paths from within the custom commands defined in `build.rs` (`REEL_CONFIG_NU`). Other nu operations in the same sandbox work.

The passing commands differ in approach:
- `reel glob` uses `cd $dir` then `glob $pattern` (relative path, directory enumeration)
- `reel grep` uses `^$env.REEL_RG_PATH` (external binary, not nu built-in)

The failing commands use:
- `reel read`: `ls $full | first` then `open $full --raw`
- `reel write`: `mkdir $parent` then `$content | save --force $full`
- `reel edit`: `open $full --raw` then `| save --force $full`

## Next steps

Investigate how nu's `ls`, `open`, and `save` resolve paths inside AppContainer. The difference between the passing and failing commands suggests the issue is in how nu built-in file commands handle absolute paths vs directory-relative operations within the sandbox.

