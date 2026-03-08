# FR-004: `SandboxCommand::forward_common_env()`

## Problem

Sandboxed processes need a minimal set of environment variables to function (PATH, HOME/USERPROFILE, TEMP/TMP, SYSTEMROOT, LANG, etc.). Every lot consumer must maintain their own list of keys to forward, read each from `std::env::var`, and call `cmd.env()` for each match.

Epic has a 19-element `FORWARDED_ENV_KEYS` array and a loop that does this. Any other consumer will write the same code.

## Proposed Solution

Add a convenience method to `SandboxCommand`:

```rust
impl SandboxCommand {
    /// Forward a standard set of environment variables from the parent
    /// process. Includes PATH, HOME, USER, LANG, LC_ALL, TERM, SHELL,
    /// TMPDIR, TMP, TEMP, SYSTEMROOT, COMSPEC, WINDIR, PROGRAMFILES,
    /// APPDATA, LOCALAPPDATA, USERPROFILE. Missing keys are silently
    /// skipped.
    pub fn forward_common_env(&mut self) -> &mut Self;
}
```

The cross-platform key list is intentional — Unix keys are no-ops on Windows and vice versa, which is simpler than `#[cfg]` splitting.

Consumers who need a custom set can still use `cmd.env(key, val)` directly.

## Scope

Small addition to `command.rs`. No new dependencies.
