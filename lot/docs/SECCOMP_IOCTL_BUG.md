# Seccomp ioctl allowlist breaks subprocess spawning inside sandbox

## Summary

Sandboxed processes that spawn child subprocesses via `std::process::Command::output()` panic on Linux when the child (or its runtime) calls an ioctl not in lot's argument-filtered allowlist. The old seccomp filter (pre-da6b0fc) allowed `prctl` and `ioctl` unconditionally. Commit da6b0fc ("Fix all 13 medium audit findings") added argument filtering, which is too narrow for programs that perform terminal I/O, use ncurses, or do shell job control.

## Affected code

`lot/src/linux/seccomp.rs` — ioctl argument filter in `build_filter()`.

Current allowlist:

| ioctl | Value | Purpose |
|-------|-------|---------|
| TCGETS | 0x5401 | Get terminal attributes |
| TIOCGWINSZ | 0x5413 | Get window size |
| TIOCGPGRP | 0x540F | Get foreground process group |
| FIONREAD | 0x541B | Bytes available for reading |
| FIOCLEX | 0x5451 | Set close-on-exec |
| FIONCLEX | 0x5450 | Clear close-on-exec |

Missing ioctls needed:

| ioctl | Value | Purpose | Used by |
|-------|-------|---------|---------|
| TCSETS | 0x5402 | Set terminal attributes (immediate) | ncurses, crossterm, termion, any raw-mode program |
| TCSETSW | 0x5403 | Set terminal attributes (drain first) | Same — polite variant |
| TCSETSF | 0x5404 | Set terminal attributes (drain+flush) | Same — mode transitions |
| TIOCSPGRP | 0x5410 | Set foreground process group | bash, zsh, any shell with job control |
| TIOCSWINSZ | 0x5414 | Set window size | tmux, screen, pty-using programs |
| TIOCGSID | 0x5429 | Get session ID | shells |
| TIOCOUTQ | 0x5411 | Output queue size | ncurses flow control |
| TCFLSH | 0x540B | Flush buffers | ncurses |
| FIONBIO | 0x5421 | Set non-blocking I/O | Programs using this instead of fcntl |

All are benign operations on the process's own fd. In lot's sandbox there is no real terminal (stdio are pipes), so most return ENOTTY. No privilege escalation path.

### Ioctls that must NOT be allowed

| ioctl | Value | Reason |
|-------|-------|--------|
| TIOCSTI | 0x5412 | Terminal input injection — classic privilege escalation vector |
| TIOCSCTTY | 0x540E | Can steal controlling terminal with CAP_SYS_ADMIN |
| TIOCCONS | 0x541D | Console redirect — requires CAP_SYS_ADMIN |
| TIOCLINUX | 0x541C | Console-specific, can inject input or change kernel loglevel |
| TIOCSETD | 0x5423 | Line discipline change — historical kernel vulnerability source |

## Reproduction

Any program that spawns a subprocess which uses ncurses or performs terminal setup will fail. Concrete example: NuShell 0.111.0 with `--config <path>`.

### Steps

1. Build lot at rev 331bb56
2. Create a sandbox policy with at least one read path
3. Spawn `nu --mcp --config <config_file>` inside the sandbox
4. Nu's config loading evaluates a `term size` command via crossterm
5. crossterm's `TIOCGWINSZ` ioctl fails (no terminal in sandbox)
6. crossterm falls back to spawning `tput cols` via `Command::output()`
7. `tput` uses ncurses, which calls `ioctl(fd, TCSETS, ...)` during `setupterm()`
8. Seccomp returns EPERM
9. `tput` fails, `Command::output()` internally unwraps the pipe read error, panicking

### Backtrace

```
Error:   x Main thread panicked.
  |-> at library/std/src/sys/process/mod.rs:61:17
  |-> called `Result::unwrap()` on an `Err` value: Os { code: 1, kind:
      PermissionDenied, message: "Operation not permitted" }

  0: __rustc::rust_begin_unwind
  1: core::panicking::panic_fmt
  2: core::result::unwrap_failed
  3: std::process::Command::output
  4: crossterm::terminal::sys::unix::tput_value
  5: crossterm::terminal::sys::unix::size
  6: <nu_command::platform::term::term_size::TermSize as Command>::run
  7: nu_engine::eval_ir::eval_call
  8: nu_engine::eval_ir::eval_instruction
  9: nu_engine::eval_ir::eval_ir_block_impl
 10: nu_engine::eval_ir::eval_ir_block
 11: nu_engine::eval::eval_block
 12: nu_cmd_base::hook::eval_hook
 13: nu_cli::util::print_pipeline
 14: nu_cli::util::eval_source
 15: nu::config_files::read_config_file
 16: nu::config_files::setup_config
 17: nu::main
```

### Environment

- Rust 1.93.1
- NuShell 0.111.0
- Ubuntu (GitHub Actions runner, ubuntu-latest)
- lot rev 331bb56

## Impact

Any sandboxed process that transitively calls `tput`, `stty`, or similar terminal utilities will fail. This includes:
- NuShell config loading (via crossterm -> tput)
- Any Rust program using crossterm that falls back to tput
- Direct ncurses-based programs

Additionally, shells running inside the sandbox with job control (bash, zsh) will fail on TIOCSPGRP. Programs using tmux/screen-style pty management will fail on TIOCSWINSZ. Programs using FIONBIO for non-blocking I/O (instead of fcntl) will fail.

## Suggested fix

Add all 9 missing ioctls from the table above to the argument filter allowlist in `build_filter()`. Update the ioctl table in `docs/DESIGN.md` to match.

## Workaround (reel)

Reel works around this by sourcing the config file via MCP evaluate after the handshake instead of passing `--config` to nu. This avoids nu's config loading code path that triggers `term size` -> crossterm -> tput.
