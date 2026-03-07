# Implementation Plan

## CI Runner Capabilities (researched 2026-03-07)

| Platform | Mechanism | Runner Status | Workaround |
|---|---|---|---|
| Linux (ubuntu-24.04, kernel 6.14) | User namespaces | Blocked by AppArmor (`apparmor_restrict_unprivileged_userns=1`) | `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` |
| Linux | seccomp-BPF | Works unprivileged | None needed |
| Linux | cgroups v2 | Available but not delegated to runner user | `sudo` to create/delegate subtree |
| macOS (macos-latest, macOS 15) | sandbox_init / Seatbelt | Available and functional (deprecated but present) | None needed |
| macOS | SIP | Cannot toggle; status varies by image | Does not affect sandbox_init |
| Windows (windows-latest) | AppContainer | Works (runner is admin, UAC disabled) | None needed |
| Windows | Job Objects | Works | None needed |
| Windows | ACLs | Works (admin privileges) | None needed |

---

## Phases

### Phase 0: Policy Validation + Test Infrastructure

**Goal:** Validate `SandboxPolicy` inputs and establish the test pattern used by all subsequent phases.

**Deliverables:**
- `SandboxPolicy::validate()` — reject empty policies, nonexistent paths, conflicting path permissions.
- `spawn()` calls `validate()` before platform dispatch.
- Unit tests for validation logic (all platforms).

**Testing:**
- Local: `cargo test` (runs on any platform — pure logic, no OS calls).
- CI: All three platform jobs run these tests.

---

### Phase 1: `probe()` Implementations

**Goal:** Replace hardcoded `available()` stubs with real detection on each platform.

**Deliverables:**
- Linux: `namespace::available()` checks `/proc/sys/kernel/unprivileged_userns_clone`. `seccomp::available()` checks `prctl(PR_GET_SECCOMP)`. `cgroup::available()` checks `/sys/fs/cgroup` mount and controller delegation.
- macOS: `seatbelt::available()` confirms `sandbox_init` symbol exists (dlsym check or unconditional true with version gate).
- Windows: `appcontainer::available()` checks Windows version >= 10 1703. `job::available()` returns true (available since XP).

**Testing:**
- Local (Windows): `cargo test` — Windows probe tests assert `appcontainer: true`, `job_objects: true`.
- CI Linux: Tests assert detection matches runner reality (namespaces may be false due to AppArmor unless sysctl applied; seccomp true; cgroups depends on delegation).
- CI macOS: Tests assert `seatbelt: true`.
- CI Windows: Tests assert `appcontainer: true`, `job_objects: true`.

---

### Phase 2: Windows Backend — Job Objects

**Goal:** Implement resource limit enforcement via Job Objects. This is self-contained and testable without AppContainer.

**Deliverables:**
- `job.rs`: `JobObject` struct wrapping `HANDLE`. Create job object, set `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` (memory limit, active process limit, kill-on-close). Assign process to job. RAII cleanup (close handle on drop).
- Unit tests: Create job, set limits, verify via query.
- Integration tests: Spawn a child process in a job with memory limit, verify it gets killed when exceeding the limit. Spawn a child that fork-bombs, verify process limit stops it.

**Testing:**
- Local: `cargo test` — all tests run directly.
- CI Windows: Same tests. Runner has full admin access.
- CI Linux/macOS: Tests are `#[cfg(target_os = "windows")]` — skipped.

---

### Phase 3: Windows Backend — AppContainer

**Goal:** Implement filesystem and network isolation via AppContainer.

**Deliverables:**
- `appcontainer.rs`:
  - `create_profile()` / `delete_profile()` — lifecycle management.
  - `grant_access()` / `revoke_access()` — ACL manipulation using the AppContainer package SID.
  - Sentinel file logic: write manifest before granting ACLs, restore on cleanup, `cleanup_stale()` scans for orphaned sentinels.
  - `SecurityCapabilities` assembly for `CreateProcessW`.
  - Network control via capability SIDs (`InternetClient`, `InternetClientServer`).
- `mod.rs`: Wire up `spawn()` — create profile, grant ACLs, create job, launch process with `STARTUPINFOEX`, return `SandboxedChild`.
- `SandboxedChild` for Windows: wraps process handle, job handle, implements `wait()`, `kill()`, `try_wait()`, `wait_with_output()`, stdin/stdout/stderr accessors. Drop impl cleans up profile + ACLs.

**Testing:**
- Local: Full integration tests:
  - Spawn sandboxed process, verify it can read allowed paths.
  - Verify it cannot read disallowed paths.
  - Verify it cannot write to read-only paths.
  - Verify network denied by default (connect fails).
  - Verify network allowed when `allow_network: true`.
  - Verify cleanup: after drop, profile is deleted, ACLs are restored.
  - Verify sentinel recovery: simulate crash, call `cleanup_stale()`.
- CI Windows: Same integration tests.
- CI Linux/macOS: Compilation-only (platform-gated code).

---

### Phase 4: Linux Backend — seccomp-BPF

**Goal:** Implement syscall filtering. Testable independently of namespaces.

**Deliverables:**
- `seccomp.rs`:
  - `available()` — real check via `prctl(PR_GET_SECCOMP)`.
  - `build_filter(policy)` — construct BPF filter using `seccompiler`. Default deny (`EPERM`). Allowlist essential syscalls. Gate network syscalls on `allow_network`.
  - `apply_filter()` — set `PR_SET_NO_NEW_PRIVS`, load filter via `seccomp()` syscall.
- Unit tests: Verify filter construction produces correct BPF program for various policies.
- Integration tests: Apply filter in a forked child, verify allowed syscalls succeed and denied syscalls return `EPERM`.

**Testing:**
- Local (if Linux): `cargo test`.
- CI Linux: Integration tests. seccomp works unprivileged — no workarounds needed.
- CI Windows/macOS: Skipped (`#[cfg]`).

---

### Phase 5: Linux Backend — Namespaces + Filesystem

**Goal:** Implement the core isolation: user/mount/pid/net/ipc namespaces, filesystem setup, pivot_root.

**Deliverables:**
- `namespace.rs`:
  - `available()` — check `/proc/sys/kernel/unprivileged_userns_clone` and attempt a test `clone(CLONE_NEWUSER)`.
  - Helper process model: fork a single-threaded child before `clone()` with namespace flags (to avoid multi-threaded restriction).
  - UID/GID mapping: write `/proc/<pid>/uid_map` and `/proc/<pid>/gid_map`.
  - Mount namespace setup: tmpfs root, bind-mount allowed paths (read-only, read-write, exec), mount `/proc`, `/dev/null`, `/dev/urandom`. `pivot_root` into new root.
  - Network namespace: empty by default (no interfaces). No veth pair in v1.
- Integration tests:
  - Spawn child in namespace, verify PID 1 inside.
  - Verify filesystem: allowed paths visible, disallowed paths absent.
  - Verify read-only enforcement on read-only paths.
  - Verify empty network namespace (no interfaces).

**Testing:**
- CI Linux: Requires `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` in workflow. Add this step before running tests.
- Local (if Linux): Same sysctl may be needed depending on distro.
- CI Windows/macOS: Skipped.

---

### Phase 6: Linux Backend — cgroups v2

**Goal:** Resource limit enforcement via cgroups v2.

**Deliverables:**
- `cgroup.rs`:
  - `available()` — check `/sys/fs/cgroup` is cgroups v2, check controller delegation for user.
  - `CgroupGuard` struct: create per-sandbox cgroup under user's delegated subtree, write `memory.max`, `pids.max`, `cpu.max`. Move child PID into cgroup. Cleanup on drop (kill remaining processes, remove cgroup dir).
- `mod.rs`: Wire cgroup into `spawn()` flow alongside namespace + seccomp.
- `SandboxedChild` for Linux: wraps child PID (via pidfd if available), namespace cleanup, cgroup guard. Implements `wait()`, `kill()`, etc.

**Testing:**
- CI Linux: Cgroup tests require `sudo` to create/delegate subtree. Add CI step: `sudo mkdir -p /sys/fs/cgroup/lot-test && sudo chown runner:runner /sys/fs/cgroup/lot-test`. Tests use this subtree.
- Local (if Linux): Same setup, or skip cgroup tests if not delegated (gate with `cgroup::available()`).
- CI Windows/macOS: Skipped.

---

### Phase 7: macOS Backend — Seatbelt

**Goal:** Implement sandboxing via Seatbelt (sandbox_init) and resource limits via setrlimit.

**Deliverables:**
- `seatbelt.rs`:
  - `generate_profile(policy)` — produce SBPL string from `SandboxPolicy`. Start with `(version 1)(deny default)`. Add file-read/file-write/process-exec/network rules. Always allow system libraries, dynamic linker, `/dev/urandom`.
  - FFI bindings: `sandbox_init()`, `sandbox_free_error()`.
  - `apply(profile)` — call `sandbox_init` in child process after fork, before exec.
  - Resource limits: `setrlimit(RLIMIT_AS, ...)` for memory.
- `mod.rs`: Wire up `spawn()` — fork helper, apply seatbelt + rlimit in helper, exec target. Return `SandboxedChild`.
- `SandboxedChild` for macOS: wraps child PID, implements wait/kill/etc.
- Unit tests: SBPL profile generation correctness for various policies.
- Integration tests:
  - Spawn child, verify allowed path readable.
  - Verify disallowed path returns permission error.
  - Verify network denied/allowed.
  - Verify memory limit enforcement.

**Testing:**
- CI macOS: Integration tests run directly. sandbox_init works unprivileged.
- Local (if macOS): Same.
- CI Linux/Windows: Skipped.

---

### Phase 8: Integration + Hardening

**Goal:** End-to-end tests, CI hardening, cleanup.

**Deliverables:**
- Cross-platform integration test suite (in `tests/` directory):
  - Common test helpers that select the right assertions per platform.
  - Full spawn-and-verify tests for each policy dimension (read, write, exec, network, resource limits).
  - Cleanup verification tests (resources freed after drop).
- CI workflow updates:
  - Linux: add sysctl + cgroup delegation steps.
  - All platforms: run integration tests with `cargo test` (platform-gated via `#[cfg]`).
  - Add `--test-threads=1` for integration tests that modify system state.
- `probe()` integration tests: verify `probe()` output matches actual capability on each runner.
- Documentation: rustdoc on public API items.

**Testing:**
- Full CI matrix: all tests on all platforms.
- Local: each platform runs its own tests.

---

## CI Workflow Changes Required

The existing CI workflow needs these additions (to be applied incrementally as phases land):

### Phase 5+ (Linux namespace tests)
```yaml
# Add before cargo test in test-linux job
- name: Enable unprivileged user namespaces
  run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
```

### Phase 6+ (Linux cgroup tests)
```yaml
# Add before cargo test in test-linux job
- name: Delegate cgroup subtree for tests
  run: |
    sudo mkdir -p /sys/fs/cgroup/lot-test
    sudo chown -R runner:runner /sys/fs/cgroup/lot-test
    echo "/sys/fs/cgroup/lot-test" > /tmp/lot-cgroup-root
```

### All phases
Integration tests that require OS mechanisms should be gated at runtime:
```rust
#[test]
fn test_something_requiring_namespaces() {
    let caps = lot::probe();
    if !caps.namespaces {
        eprintln!("skipping: namespaces not available");
        return;
    }
    // ... actual test
}
```

This avoids hard failures when a mechanism is unavailable, while still running the test when the environment supports it.

---

## Phase Dependencies

```
Phase 0 (validation)
  |
  v
Phase 1 (probe)
  |
  +---> Phase 2 (Win job) ---> Phase 3 (Win appcontainer)
  |
  +---> Phase 4 (Linux seccomp) ---> Phase 5 (Linux namespaces) ---> Phase 6 (Linux cgroups)
  |
  +---> Phase 7 (macOS seatbelt)
  |
  All ---> Phase 8 (integration + hardening)
```

Phases 2-3, 4-6, and 7 are independent of each other and can be developed in any order. Within each platform track, phases are sequential.

---

## Recommended Order

1. Phase 0 — foundation for everything.
2. Phase 1 — unblocks platform testing.
3. Phase 2 + 3 — Windows. Testable on current dev machine.
4. Phase 4 + 5 + 6 — Linux. Testable via CI.
5. Phase 7 — macOS. Testable via CI.
6. Phase 8 — polish and harden.
