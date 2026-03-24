# Known Issues

## #89 — Config loaded via MCP evaluate instead of --config (lot seccomp workaround)

**Status**: Waiting on lot fix

Nu's `--config` flag triggers config loading code that evaluates `term size`, which calls `crossterm::terminal::size()`, which falls back to spawning `tput` via `Command::output()`. Inside lot's sandbox on Linux, `tput` calls an ioctl (likely TCSETS/TCSETSW for ncurses `setupterm()`) that is not in lot's argument-filtered seccomp allowlist. The seccomp default action returns EPERM, Rust's stdlib unwraps the pipe read error, and nu's main thread panics.

**Workaround**: Config file is sourced via MCP `evaluate` after the handshake instead of `--config` flag. This avoids nu's config loading code path entirely.

**Root cause**: Lot rev 331bb56 changed `prctl` and `ioctl` from unconditionally allowed to argument-filtered. The ioctl allowlist includes TCGETS (read terminal attrs) but not TCSETS/TCSETSW/TCSETSF (write terminal attrs). Bug report filed at `lot/docs/SECCOMP_IOCTL_BUG.md`.

**Resolution**: Once lot adds the missing ioctls to its allowlist, revert to `--config` flag and remove the MCP evaluate workaround in `spawn_nu_process`.
