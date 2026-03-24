# Spec: Remove Resource Limits

## Summary

Remove the `ResourceLimits` feature entirely — struct, builder methods, platform enforcement, CLI config, tests, and documentation references.

## Motivation

Resource limits are not central to lot's purpose (process sandboxing). Platform support is non-uniform — Linux cgroups v2 requires delegation and doesn't support CPU time limits, macOS `RLIMIT_AS` is unreliable on Apple Silicon, and Windows job objects have different semantics from both. The feature carries significant complexity (cgroup lifecycle, rlimit edge cases, job object limit plumbing) for limited gain. Removing it shrinks the API surface, reduces platform-specific code, and eliminates a class of CI setup requirements (cgroup delegation).

## Files to Modify

### 1. `lot/src/policy.rs`

- Delete `ResourceLimits` struct (lines 364–415), including `has_any()` and `validate()` methods.
- Remove `limits: ResourceLimits` field from `SandboxPolicy`.
- Remove `limits()` accessor from `SandboxPolicy`.
- Remove `limits` field initialization in `SandboxPolicy` construction (in `SandboxPolicyBuilder::build()`).
- Remove the `limits.validate()?` call in policy validation.

### 2. `lot/src/policy_builder.rs`

- Remove `limits: ResourceLimits` field from `SandboxPolicyBuilder`.
- Delete builder methods: `max_memory_bytes()`, `max_processes()`, `max_cpu_seconds()`.
- Remove `limits` from the struct constructed in `build()`.

### 3. `lot/src/lib.rs`

- Remove `ResourceLimits` from `pub use policy::{ResourceLimits, SandboxPolicy}`.

### 4. Linux: `lot/src/linux/mod.rs`

- Remove the `policy.limits().has_any()` check and `CgroupGuard` creation in `spawn()`.
- Remove `cgroup_guard: Option<CgroupGuard>` field from `LinuxSandboxedChild`.
- Remove cgroup-related drop logic.

### 5. Linux: `lot/src/linux/cgroup.rs`

- Delete entire file.
- Remove `mod cgroup;` from `lot/src/linux/mod.rs`.

### 6. macOS: `lot/src/unix.rs`

- Delete `set_rlimit()` helper function.
- Delete `apply_resource_limits()` function.

### 7. macOS: `lot/src/macos/mod.rs`

- Remove `apply_resource_limits()` call from child setup in `spawn()`.
- Remove `STEP_RLIMIT` constant if it becomes unused.

### 8. Windows: `lot/src/windows/job.rs`

- Remove resource-limit fields from `set_limits()`: `ProcessMemoryLimit`, `ActiveProcessLimit`, `PerJobUserTimeLimit` and associated flag logic.
- `set_limits()` still needs to exist (or be inlined) to set `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and UI restrictions. Rename to `configure()` or similar since it no longer sets "limits".
- Remove `ResourceLimits` parameter; the function takes no policy input.

### 9. Windows: `lot/src/windows/appcontainer.rs`

- Update `spawn_with_sentinel()` call to `job.set_limits()` — adapt to new signature (no `ResourceLimits` arg).

### 10. CLI: `lot-cli/src/config.rs`

- Delete `LimitsConfig` struct.
- Remove `limits` field from the top-level config struct.
- Remove `max_memory_bytes` / `max_processes` / `max_cpu_seconds` application in `build_policy()`.
- Update/remove config deserialization tests that use `limits:`.

### 11. Tests: `lot/tests/integration.rs`

- Delete `test_memory_limit_enforcement()`.
- Delete `test_memory_limit_returns_error_on_macos()`.

### 12. Tests: `lot/tests/common/mod.rs`

- Delete `memory_hog_command()` helper (and its platform-specific imports if they become unused).

### 13. Example: `lot/examples/resource_limits.rs`

- Delete entire file.

### 14. Documentation

- `docs/DESIGN.md`: Remove ResourceLimits references from:
  - Linux section: cgroup v2 resource limits, memory.max, pids.max, cpu.max discussion.
  - macOS section: `setrlimit` discussion, `RLIMIT_AS` caveat, `RLIMIT_NPROC`, `RLIMIT_CPU`.
  - Windows section: `JOB_OBJECT_LIMIT_PROCESS_MEMORY`, `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`, `JOB_OBJECT_LIMIT_JOB_TIME` from Job Objects section.
  - Graceful degradation table: remove "Linux: cgroups v2 not mounted or not delegated" row.
  - Integration tests list: remove memory limit and process limit test descriptions.
  - CI table: remove cgroup delegation from Linux test setup.
  - Testing strategy: remove memory limit and process limit test descriptions.
- `docs/STATUS.md`: Update to reflect removal.
- `docs/ISSUES.md`: Remove any findings that reference resource limits (they become moot).

### 15. CI: `.github/workflows/`

- Remove cgroup delegation setup from Linux test job (if resource limits were the only reason for it).

### 16. `docs/ISSUES.md`

Remove 11 findings that exist solely because of resource limits or cgroup.rs:

| # | Reason |
|---|--------|
| 18 | `apply_resource_limits` test coverage — function removed |
| 19 | `signal_all()` fallback in cgroup.rs — file deleted |
| 22 | `libc::kill()` return in `signal_all()` — file deleted |
| 23 | `fs::remove_dir` in cgroup.rs Drop — file deleted |
| 26 | macOS `set_rlimit`/`apply_resource_limits` placement — functions removed |
| 38 | cgroup struct doc comment — file deleted |
| 52 | `signal_all` naming in cgroup.rs — file deleted |
| 55 | `has_writable_delegation` duplication in cgroup.rs — file deleted |
| 69 | `procs_path` recomputed in cgroup.rs — file deleted |
| 89 | No test for cgroup integration path in spawn — path removed |
| 92 | `current_cgroup_path()`/`pid_in_cgroup()` parsing — file deleted |

Renumber remaining findings after removal. Update header counts.

## Dependency Order

1. Delete example file and test helpers/tests (no dependents).
2. Remove CLI config (depends on builder methods).
3. Remove platform enforcement (Linux cgroup, macOS rlimit, Windows job limits).
4. Remove builder methods and policy struct fields.
5. Remove public re-export.
6. Update documentation.

## What Stays

- **Windows Job Object** — `KILL_ON_JOB_CLOSE` and UI restrictions remain. The job object is still created and assigned; only resource-limit fields are removed.
- **Linux namespaces, seccomp** — unaffected.
- **macOS seatbelt** — unaffected.
- **`probe()`** — remove any cgroup-related probe checks if they exist solely for resource limits.
