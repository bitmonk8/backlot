# Seccomp ioctl allowlist breaks subprocess spawning inside sandbox

## Summary

Sandboxed processes that spawn child subprocesses via `std::process::Command::output()` panic on Linux when the child (or its runtime) calls an ioctl not in lot's argument-filtered allowlist. The old seccomp filter (pre-331bb56) allowed `prctl` and `ioctl` unconditionally. The new filter restricts them to specific operations, which is too narrow for programs that perform terminal I/O or use ncurses.

## Affected code

`lot/src/linux/seccomp.rs` — ioctl argument filter (lines ~200-215 at rev 331bb56).

Current allowlist:

| ioctl | Value | Purpose |
|-------|-------|---------|
| TCGETS | 0x5401 | Get terminal attributes |
| TIOCGWINSZ | 0x5413 | Get window size |
| TIOCGPGRP | 0x540F | Get process group |
| FIONREAD | 0x541B | Bytes available for reading |
| FIOCLEX | 0x5451 | Set close-on-exec |
| FIONCLEX | 0x5450 | Clear close-on-exec |

Missing ioctls likely needed:

| ioctl | Value | Purpose |
|-------|-------|---------|
| TCSETS | 0x5402 | Set terminal attributes (immediate) |
| TCSETSW | 0x5403 | Set terminal attributes (drain first) |
| TCSETSF | 0x5404 | Set terminal attributes (drain+flush) |

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

## Suggested fix

Add TCSETS (0x5402), TCSETSW (0x5403), and TCSETSF (0x5404) to the ioctl argument filter allowlist in `build_filter()`. These are the "set terminal attributes" counterparts to the already-allowed TCGETS (0x5401).

Alternatively, if terminal-write ioctls are intentionally blocked as a security measure, document this limitation and provide guidance for consumers (e.g., override PATH to exclude tput, or avoid programs that perform terminal setup).

## Workaround (reel)

Reel works around this by sourcing the config file via MCP evaluate after the handshake instead of passing `--config` to nu. This avoids nu's config loading code path that triggers `term size` -> crossterm -> tput.
