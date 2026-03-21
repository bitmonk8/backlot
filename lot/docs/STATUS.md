# Project Status

## Current Phase

**Issue remediation.** Implementation complete across all platforms. Working through audit findings in `docs/ISSUES.md`.

## Goal

Clear all issues in `docs/ISSUES.md`.

## Next Work

**Group 1: Sandbox Enforcement Correctness** — three issues affecting sandbox safety:
- 1.1 `kill_and_reap` deadlock — SIGKILL never sent (`unix.rs`)
- 1.2 `bind_mount_readonly` submounts remain writable (`linux/namespace.rs`)
- 1.3 `escape_sbpl_path` produces invalid SBPL escapes (`macos/seatbelt.rs`)

## Completed

- Full implementation (Linux, macOS, Windows)
- CI pipeline (format, clippy, build, test on all 3 platforms)

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
