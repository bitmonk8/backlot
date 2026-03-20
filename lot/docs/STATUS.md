# Project Status

## Current Phase

**Issue resolution.** Implementation complete across all platforms. Audit findings documented in `ISSUES.md` across remaining groups (10–12). Goal: clear all issues.

## Resolved

- **Group 1: macOS Seatbelt Silent Security Bypass**
- **Group 2: Windows Process Creation & Stdio Handle Safety**
- **Group 3: Windows Sentinel & ACL Recovery Robustness**
- **Group 4: Unix Process Lifecycle Safety**
- **Group 5: Windows ACL/DACL Error Propagation**
- **Group 6: Linux Cgroup & Mount Robustness**
- **Group 7: Test Coverage Gaps**
- **Group 8: CLI Environment Variable Ordering**
- **Group 9: DESIGN.md Documentation Accuracy**

## Next Work

1. Groups 10–12 in `ISSUES.md`, ordered by descending impact.

After all issues resolved:
- First real-world usage / `lot run` testing
- crates.io publish (metadata ready, needs public repository URL)

## CI Notes

Ubuntu 24.04 requires `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` to allow unprivileged user namespace creation. The CI already sets this.
