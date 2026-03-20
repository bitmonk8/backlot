# Project Status

## Current Phase

**Issue resolution.** Implementation complete across all platforms. Audit findings documented in `ISSUES.md` across remaining groups (7–12). Goal: clear all issues.

## Resolved

- **Group 1: macOS Seatbelt Silent Security Bypass**
- **Group 2: Windows Process Creation & Stdio Handle Safety**
- **Group 3: Windows Sentinel & ACL Recovery Robustness**
- **Group 4: Unix Process Lifecycle Safety**
- **Group 5: Windows ACL/DACL Error Propagation**
- **Group 6: Linux Cgroup & Mount Robustness**

## Next Work

1. **Group 7: Test Coverage Gaps** — Zero coverage on public API surfaces, assertion-free tests. (2 High, 5 Medium)
2. Groups 8–12 in `ISSUES.md`, ordered by descending impact.

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
