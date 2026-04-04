# Claude Code Configuration

## Testing
**No silent test skipping**: Tests must never silently pass when prerequisites are missing. Use `assert!`/`panic!` to fail loudly, not early `return` or skip macros. A skipped test is a lie — it reports success when nothing was verified.

## Windows AppContainer Temp Directories
Epic uses lot for process sandboxing. Any path granted to a sandboxed process (via `lot::grant_appcontainer_prerequisites`, `SandboxPolicyBuilder`, or `lot::spawn`) must not live under system temp (`%TEMP%`, typically `C:\Users\{user}\AppData\Local\Temp`). The ancestor `C:\Users` requires elevation for AppContainer traverse ACE grants, causing `PrerequisitesNotMet` errors without elevated `lot setup`. Use project-local gitignored directories instead. In tests, use `TempDir::new_in()` with a project-local path, not `TempDir::new()`.
