# Project Status

## Current Phase

**Issue resolution.** Implementation complete across all platforms. Audit findings documented in `ISSUES.md` across 11 remaining groups (2–12). Goal: clear all issues.

## Resolved

- **Group 1: macOS Seatbelt Silent Security Bypass** — `generate_profile` and `append_sbpl_rule` now return `Result`, propagating path-encoding errors instead of silently dropping rules. `canonicalize` failure in `spawn()` returns `SandboxError::Setup` instead of silently falling back.

## Next Work

1. **Group 2: Windows Process Creation & Stdio Handle Safety** — Console handle corruption on error paths and invalid handles passed to child processes. (1 Critical, 1 High)
2. Groups 3–12 in `ISSUES.md`, ordered by descending impact.

After all issues resolved:
- First real-world usage / `lot run` testing
- crates.io publish (metadata ready, needs public repository URL)

## CI Status

| Job | Status |
|---|---|
| Format | Pass |
| Build | Pass |
| Clippy (macOS) | Pass |
| Clippy (Linux) | Pass |
| Clippy (Windows) | Pass |
| Test (macOS) | Pass |
| Test (Linux) | Pass |
| Test (Windows) | Pass |

### Note: ubuntu-24.04 AppArmor

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.

