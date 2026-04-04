# Claude Code Configuration

## Testing
**No silent test skipping**: Tests must never silently pass when prerequisites are missing. Use `assert!`/`panic!` to fail loudly, not early `return` or skip macros. A skipped test is a lie — it reports success when nothing was verified.

## Windows AppContainer Temp Directories
Crates that use lot for process sandboxing (lot, reel, epic) must not place sandbox-granted paths under system temp (`%TEMP%`). The ancestor `C:\Users` requires elevation for AppContainer traverse ACE grants. Use project-local gitignored directories instead. In tests, use `TempDir::new_in()` with a project-local path, not `TempDir::new()`.
