# Project Status

## Current Phase

**Issue resolution.** Implementation complete across all platforms. 41 audit findings documented in `ISSUES.md` across 11 groups. Goal: clear all issues.

## Next Work

1. **Group 1: macOS Seatbelt Silent Security Bypass** — `seatbelt.rs` silently drops deny rules on path escape failure; `canonicalize` failure swallowed. (1 Critical, 1 Medium)
2. Groups 2–11 in `ISSUES.md`, ordered by descending impact.

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

