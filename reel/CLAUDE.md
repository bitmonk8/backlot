# Claude Code Configuration

## Windows AppContainer Temp Directories
Reel uses lot for process sandboxing. Any path granted to a sandboxed process (via `SandboxPolicyBuilder`) must not live under system temp (`%TEMP%`, typically `C:\Users\{user}\AppData\Local\Temp`). The ancestor `C:\Users` requires elevation for AppContainer traverse ACE grants, causing `PrerequisitesNotMet` errors without elevated `lot setup`. Use project-local gitignored directories instead — their ancestors are user-owned and do not require elevation. In tests, use `TempDir::new_in()` with a project-local path, not `TempDir::new()`.
