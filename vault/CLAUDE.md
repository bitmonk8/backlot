# Claude Code Configuration

## Testing
**No silent test skipping**: Tests must never silently pass when prerequisites are missing. Use `assert!`/`panic!` to fail loudly, not early `return` or skip macros. A skipped test is a lie — it reports success when nothing was verified.
