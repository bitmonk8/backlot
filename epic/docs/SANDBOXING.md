# Sandboxing

> **Operational correctness sandboxing has been delegated to the [lot](https://github.com/bitmonk8/lot) project** — a standalone cross-platform process sandboxing library (Seatbelt on macOS, AppContainer on Windows, namespaces + seccomp + cgroups v2 on Linux). Epic will consume lot as a dependency. The Frida-based approach described in Concern 2 below is superseded. See [LOT_SPEC.md](LOT_SPEC.md) for the lot design spec.

Epic sandboxing addresses two distinct concerns with fundamentally different solutions.

## Concern 1: Security Isolation

### Problem

LLM agents execute arbitrary shell commands and file operations. A compromised or misbehaving agent could read secrets, exfiltrate data, modify system files, or attack the network. No amount of in-process checking can fully prevent a determined or unexpected escape.

### Approach: User-Managed VM/Container

Epic does **not** attempt OS-level sandboxing. The only robust security boundary is running epic itself inside an appropriately configured virtual environment:

- **Docker / Podman** — Bind-mount only the project directory. Restrict network access. Drop capabilities.
- **VM** — Full isolation. Suitable for high-security contexts.
- **systemd-nspawn** — Lightweight Linux container option.

Epic's responsibility is **guidance, not enforcement**:

1. **Startup detection** (implemented) — `sandbox::detect_virtualization()` performs best-effort checks for container/VM environment. If not detected, emits a warning to stderr. Suppressible via `--no-sandbox-warn` or `EPIC_NO_SANDBOX_WARN`.
2. **Documentation** — Provide recommended container configurations (Dockerfile/Compose examples, bind-mount guidance, network policy).

### Detection Signals

| Environment | Signal |
|---|---|
| Docker | `/.dockerenv` exists, or `/proc/1/cgroup` contains `docker`/`containerd` |
| Podman | `/.dockerenv` or `/run/.containerenv` exists |
| systemd-nspawn | `systemd-detect-virt` returns `systemd-nspawn` |
| Linux VM (general) | `systemd-detect-virt` returns a hypervisor name |
| macOS VM | `sysctl kern.hv_vmm_present` returns 1 |
| WSL | `/proc/version` contains `Microsoft` or `WSL` |
| Windows VM | PowerShell `(Get-CimInstance Win32_ComputerSystem).Model` contains VM vendor string |
| Windows (note) | Unlike Linux/macOS, no reliable signal for running inside a Windows container |

None of these are foolproof. Detection is best-effort — a false negative means the warning shows unnecessarily, which is acceptable.

### Non-Goals

- Epic will not implement namespace/seccomp/chroot/landlock isolation.
- Epic will not refuse to run outside a container — only warn.

---

## Concern 2: Correct Epic Operation

### Problem

Each epic operation (assessment, decomposition, leaf execution, verification, etc.) has a defined contract for what it should access. Currently, these contracts are expressed in two ways:

1. **`ToolGrant` bitflags** — Control which tool categories (READ, WRITE, BASH) are offered to the agent per phase. The agent only sees tool definitions matching its grant.
2. **`safe_path()` validation** — All file tool operations resolve paths and check containment within the project root.

These are necessary but insufficient:

- **`ToolGrant` is prompt-level enforcement.** Epic only offers permitted tools in the Flick config, but the bash tool can do anything — read files, write files, access network, spawn processes. An agent with BASH grant has effectively unrestricted access regardless of what other grants are withheld.
- **`safe_path()` only covers epic's own tool implementations.** Commands run through bash bypass it entirely.
- **No per-task scoping.** All tasks with the same phase get the same grant. A leaf task implementing a change to `src/parser.rs` has write access to the entire project tree.

The goal is to detect violations of the operational contract **as close to the source as possible**, so that:

- A read-only phase that attempts a write is caught immediately, not discovered later via corrupted state.
- A leaf task that modifies files outside its expected scope is flagged.
- Violations produce clear diagnostics tied to the task and phase that caused them.

### Approach: Frida-Based Runtime Interception

Use [Frida](https://frida.re/) — specifically **frida-gum** (in-process interception) and **frida-core** (child process attachment) — to hook filesystem and process syscalls and enforce per-phase policies at runtime.

#### Why Frida

- **Cross-platform** — Linux, macOS, Windows. Covers all epic target platforms.
- **Rust bindings** — [`frida-gum`](https://crates.io/crates/frida-gum) and [`frida-core`](https://crates.io/crates/frida-core) crates exist and are maintained.
- **In-process hooking** — Since Flick is a library crate, agent tool execution happens in epic's own process. frida-gum can intercept syscalls without spawning external processes.
- **Child process coverage** — The bash tool spawns subprocesses. frida-core can attach to/inject into child processes to extend policy enforcement.
- **Actively maintained and popular** — Large community, regular releases, battle-tested in security tooling.

#### Policy Model

Each task phase defines an **access policy**:

| Phase | Filesystem Read | Filesystem Write | Process Spawn | Network |
|---|---|---|---|---|
| Assess | Project root (recursive) | None | None | None |
| Decompose | Project root (recursive) | None | None | None |
| Execute (leaf) | Project root (recursive) | Scoped to task-relevant paths | Yes (project root cwd) | TBD |
| Verify | Project root (recursive) | None | Yes (verification commands only) | TBD |
| Fix (leaf) | Project root (recursive) | Scoped to task-relevant paths | Yes (project root cwd) | TBD |
| Checkpoint | None (structured output only) | None | None | None |
| Recovery | Project root (recursive) | None | None | None |

Policies are parameterized per-task:
- **Read set** — Usually the full project root.
- **Write set** — Derived from the task's scope (assessment output, magnitude estimate, or explicit file list from decomposition).
- **Spawn rules** — Which commands are permitted (verification commands from `epic.toml`, general shell for execution phases).

#### Architecture (Preliminary)

```
┌─────────────────────────────────────────────┐
│  Orchestrator                               │
│                                             │
│  ┌─────────────┐    ┌───────────────────┐   │
│  │ Task + Phase │───>│ Policy Builder    │   │
│  │              │    │ (read/write/exec  │   │
│  │              │    │  sets per task)   │   │
│  └─────────────┘    └───────┬───────────┘   │
│                             │               │
│                     ┌───────▼───────────┐   │
│                     │ Frida Interceptor  │   │
│                     │ (gum: in-process)  │   │
│                     │ (core: children)   │   │
│                     └───────┬───────────┘   │
│                             │               │
│                     ┌───────▼───────────┐   │
│                     │ Violation Handler  │   │
│                     │ - Log violation    │   │
│                     │ - Fail the tool    │   │
│                     │   call             │   │
│                     │ - Report to        │   │
│                     │   orchestrator     │   │
│                     └───────────────────┘   │
└─────────────────────────────────────────────┘
```

**Lifecycle:**
1. Before each agent call, the orchestrator builds a policy from the task's phase and scope.
2. The policy is installed as a Frida interceptor (hooking `open`, `openat`, `write`, `execve`, etc.).
3. During tool execution, intercepted calls are checked against the policy.
4. Violations are handled (block the call, return error, log the violation).
5. After the agent call completes, the interceptor is removed or updated for the next phase.

#### What Gets Hooked

Preliminary list of syscalls/functions to intercept:

| Category | Linux | macOS |
|---|---|---|
| File open | `open`, `openat`, `openat2` | `open`, `openat` |
| File write | Detected via flags on open (`O_WRONLY`, `O_RDWR`, `O_CREAT`, `O_TRUNC`) | Same |
| File rename/delete | `rename`, `renameat`, `unlink`, `unlinkat`, `rmdir` | Same |
| Process spawn | `execve`, `execveat`, `posix_spawn` | `execve`, `posix_spawn`, `posix_spawnp` |
| Network | `connect`, `sendto` | `connect`, `sendto` |

#### Violation Handling

Options (to be decided):

1. **Block + error** — The syscall returns an error (EPERM). The tool call fails. The agent sees an error message explaining the policy violation.
2. **Allow + log** — The syscall proceeds, but the violation is recorded. The orchestrator can decide to fail the task after the fact.
3. **Configurable** — Strict mode (block) vs. audit mode (log only). Useful for initial rollout.

Recommendation: Start with **audit mode** (allow + log) to gather data on false positives, then switch to **block + error** as the default once policies are validated.

### Open Questions

1. **Child process injection latency** — frida-core attachment to spawned processes has inherent latency. A short-lived subprocess (e.g., `cat file`) might complete before the interceptor is installed. Is there a way to hook `execve` in the parent process to intercept before the child runs? Or do we need to use `ptrace`-style pre-exec hooks?

2. **Write set derivation** — How precisely can we determine the expected write set for a leaf task? The assessment provides a magnitude estimate (lines added/modified/deleted) but not always an explicit file list. Options:
   - Use file list from decomposition if available.
   - Use the magnitude estimate to set a budget, not a file list.
   - Start permissive (full project root write access during execute phase) and tighten later.

3. **Network policy** — Should execution phases have network access? Build/test commands may need to download dependencies. Options:
   - Allow all network during execute/verify, block during assess/decompose/checkpoint.
   - Allow only during verify (build/test), block during execute.
   - No network restrictions (leave to the VM/container layer).

4. **Performance impact** — Intercepting every file open in a build process (which may open thousands of files) adds overhead. Need to benchmark. Likely negligible compared to LLM latency, but verification phases running `cargo build` could be sensitive.

5. **Frida-gum Rust bindings maturity** — Need to verify: do the current Rust bindings support the interceptor API fully? Can we set per-thread or per-context policies (since epic is async/tokio, multiple things may share threads)?

6. **Tokio thread pool interaction** — Frida-gum interceptors are typically per-thread. Tokio moves tasks across threads. Need to ensure the policy is enforced regardless of which thread executes the tool. May need to install interceptors on all worker threads, or use a different strategy.

7. **Graceful degradation** — If Frida fails to initialize (unsupported platform, missing runtime), should epic refuse to run or fall back to current behavior (prompt-level grants + safe_path only) with a warning?

### Existing Enforcement (Retained)

Frida interception is an additional layer. Existing mechanisms remain:

- **`ToolGrant` bitflags** — Continue controlling which tools are offered to agents per phase. This is the first line of defense and prevents the agent from even attempting disallowed operations in most cases.
- **`safe_path()` containment** — Continue validating paths in epic's own tool implementations. Catches obvious violations before they reach the filesystem.
- **`required_grant()` check in `execute_tool()`** — Rejects tool calls that don't match the current grant. Prevents the agent from calling tools it wasn't offered.

Frida catches what these layers miss: operations performed through bash, unexpected filesystem access patterns, and violations by child processes.

### Implementation Phases (Future)

This section is a rough ordering, not a commitment:

1. **Prototype** — Confirm frida-gum Rust bindings work for basic `open`/`openat` interception on macOS and Linux. Measure overhead.
2. **In-process file policy** — Hook file open/write syscalls in epic's process. Enforce read/write sets based on current task phase. Covers epic's own tool implementations (belt + suspenders with `safe_path`).
3. **Child process policy** — Use frida-core to extend enforcement to bash-spawned subprocesses.
4. **Audit mode** — Log violations without blocking. Run against real workloads to validate policies and identify false positives.
5. **Enforcement mode** — Block violations. Make this the default.
6. **Network policy** — Add connect/sendto interception if needed.

---

## Alternative Evaluation: OS-Native Sandboxing Crates

Evaluated as potential replacements for Frida-based interception. The goal: sandbox child processes spawned by epic's bash tool using kernel-enforced mechanisms rather than runtime syscall hooking.

### birdcage (phylum-dev/birdcage)

**What it is:** Cross-platform embeddable sandboxing library. Linux: user/mount/PID/IPC/network namespaces + seccomp. macOS: Seatbelt (`sandbox_init`).

**API:** Simple — create `Birdcage`, add `Exception` variants (`Read`, `WriteAndRead`, `ExecuteAndRead`, `Environment`, `Networking`), call `spawn(Command)`.

```rust
let mut sandbox = Birdcage::new();
sandbox.add_exception(Exception::Read("/project".into()))?;
sandbox.add_exception(Exception::ExecuteAndRead("/bin/sh".into()))?;
let child = sandbox.spawn(command)?;
```

**Verdict: GPL-3.0 license is the primary blocker. Threading constraint is workable.**

| Concern | Detail |
|---|---|
| GPL-3.0 license | Copyleft. Constrains downstream licensing. **Primary blocker.** |
| Sandboxes calling process | Applies sandbox to the calling process before spawning the child. The orchestrator itself would be restricted. Requires a fork-based wrapper: epic forks a short-lived helper process, the helper calls `Birdcage::spawn()`, and the orchestrator communicates via pipes. Adds complexity but is architecturally sound. |
| Linux single-threaded at spawn | `spawn()` asserts `thread_count() == 1` on Linux only (not macOS). This is a kernel constraint — `CLONE_NEWUSER` requires single-threaded. The assertion is point-in-time at `spawn()`, not a lifecycle requirement. The fork-helper approach (above) satisfies this since the forked helper is single-threaded. |
| No Windows support | Linux and macOS only. |
| Kernel version | Linux 5.12+ required (mount_setattr). |

The fork-helper pattern solves both the threading and calling-process-sandboxing concerns. The GPL-3.0 license is the real obstacle — it would impose copyleft on epic or require an alternative licensing arrangement.

### rappct (cpjet64/rappct)

**What it is:** Rust wrapper around Windows AppContainer and LPAC (Low Privilege AppContainer) APIs. Launches child processes inside a kernel-enforced AppContainer boundary with configurable capabilities and ACL-based resource access.

**API:** Profile creation → capability assembly → sandboxed process launch → ACL grants for accessible paths.

```rust
// 1. Create AppContainer profile
let profile = AppContainerProfile::ensure("epic.sandbox", "epic", None)?;

// 2. Build security capabilities (empty = deny all by default)
let caps = SecurityCapabilitiesBuilder::new(&profile.sid).build()?;

// 3. Grant filesystem access to specific paths via ACL
grant_to_package(
    ResourcePath::Directory("/project/src".into()),
    &profile.sid,
    AccessMask::FILE_GENERIC_READ,
)?;

// 4. Launch sandboxed child
let opts = LaunchOptions {
    exe: "C:/Windows/System32/cmd.exe".into(),
    cmdline: Some("/C cargo build".into()),
    stdio: StdioConfig::Pipe,
    join_job: Some(JobLimits {
        memory_bytes: Some(512 * 1024 * 1024),
        kill_on_job_close: true,
        ..Default::default()
    }),
    ..Default::default()
};
let child = launch_in_container_with_io(&caps, &opts)?;
let exit_code = child.wait(None)?;
```

**Verdict: Viable for Windows.**

| Aspect | Assessment |
|---|---|
| Sandboxes child only | Correct architecture — orchestrator retains full access. |
| Kernel-enforced | AppContainer is a Windows kernel security boundary. No TOCTOU. |
| Tokio compatible | Launches child via `CreateProcessW`. No thread restrictions on caller. |
| Filesystem scoping | ACL grants per path. Read-only, read-write, or full access. Deny-by-default. |
| Network control | AppContainer denies network by default; opt-in via `InternetClient` capability. |
| Resource limits | Job objects provide memory/CPU caps. `kill_on_job_close` for cleanup. |
| I/O capture | Pipe stdin/stdout/stderr for tool output collection. |
| License | MIT. No constraints. |
| Maturity | v0.13, ~280 commits, active development. Low adoption (5 stars, ~2k downloads). |
| Limitation | Windows only. Minimum Windows 10 1703 for LPAC. |

**Concerns:**
- ACL grants are persistent filesystem metadata changes — must be cleaned up after sandbox teardown. Profile deletion helps but ACL entries on project files may linger.
- Low community adoption means less battle-testing. API may still change (pre-1.0).
- AppContainer profiles are system-global; concurrent epic instances need unique profile names.

### Comparison Matrix

| | Frida | birdcage | rappct |
|---|---|---|---|
| **Linux** | Yes | Yes | No |
| **macOS** | Yes | Yes | No |
| **Windows** | Yes | No | Yes |
| **Sandboxes children** | Yes (frida-core) | No (sandboxes caller) | Yes |
| **Tokio compatible** | Unknown (see Q6) | No | Yes |
| **Enforcement level** | Syscall interception | Kernel namespaces | Kernel AppContainer |
| **TOCTOU risk** | Yes (hook latency) | No (kernel-enforced) | No (kernel-enforced) |
| **License** | Custom/LGPL | GPL-3.0 | MIT |
| **Complexity** | High (JS scripts, injection) | Low (but unusable) | Medium |

### Possible Combined Strategy

Instead of Frida for all platforms, use native OS sandboxing per platform:

| Platform | Mechanism | Crate / API |
|---|---|---|
| Windows | AppContainer / LPAC | `rappct` |
| Linux | Landlock LSM (5.13+) | `landlock` crate (rust-landlock) |
| macOS | Seatbelt (`sandbox_init`) | Direct FFI (2 functions) |

**Advantages over Frida:**
- Kernel-enforced on all platforms — no TOCTOU, no hook latency, no race conditions.
- No runtime injection complexity. No JS scripting layer.
- No concern about tokio thread pool interaction (sandboxing applies to child process, not caller).
- Simpler dependency chain. No Frida runtime to bundle.

**Disadvantages vs Frida:**
- Three platform-specific implementations instead of one (mostly) cross-platform tool.
- Landlock requires Linux 5.13+. Older kernels would need a fallback (seccomp-bpf or no enforcement).
- macOS Seatbelt (`sandbox_init`) is deprecated by Apple (still functional, used by major apps, but no new features).
- Per-task write scoping on Linux/macOS requires careful path management (Landlock rules, Seatbelt profiles).

### yule-sandbox (visualstudioblyat/yule)

**What it is:** A cross-platform sandbox crate from the Yule project (ML inference runtime). Implements per-platform native sandboxing behind a unified `Sandbox` trait. Not published to crates.io — it's a workspace member of the yule monorepo.

**Architecture:** `apply_to_current_process(&self, config) -> Result<SandboxGuard>` — applies sandbox to the calling process (designed for fork-then-sandbox). `spawn()` is declared but unimplemented on all platforms.

**Platform backends:**

| Platform | Layers | Crates |
|---|---|---|
| Linux | rlimit → Landlock (ABI V3, `restrict_self`) → seccomp-BPF (syscall allowlist) | `landlock` 0.4, `seccompiler` 0.5, `libc` |
| macOS | rlimit → Seatbelt (`sandbox_init` FFI, SBPL profile generation) | `libc` |
| Windows | Job object (memory limit, UI restrictions, `ActiveProcessLimit=1`) | `windows-sys` 0.59 |

**Key design properties:**
- **No single-threaded requirement.** Landlock's `restrict_self()` and macOS `sandbox_init()` work from any thread.
- **Graceful degradation.** Landlock failure → warning, continues. Seccomp failure → warning, continues.
- **Layered enforcement.** Linux applies three independent layers; each can fail independently.
- **Seccomp allowlist is explicit.** ~50 syscalls allowed by default, networking and ioctl gated by config flags. Default action: `EPERM` (not kill — debuggable).
- **Seatbelt profile is ~70 lines.** Deny-default, explicit allows for system libs, model path, optional GPU/network.

**Relevance to Epic:**

This validates the combined-strategy approach. Yule demonstrates that Landlock + seccomp (Linux) and Seatbelt FFI (macOS) are practical to implement directly — no need for birdcage's namespace complexity or Frida's runtime injection.

**Gaps vs Epic's needs:**
- **Windows backend is minimal.** Job objects only (memory, UI, process count). No filesystem or network restrictions. No AppContainer. rappct would be needed for Windows filesystem sandboxing.
- **Config is ML-focused.** `SandboxConfig` has `model_path`, `allow_gpu`. Epic needs per-phase read/write path sets. The policy model needs redesign, not reuse.
- **No write-path distinction.** Only read-only paths. Epic needs read-only vs read-write per path.
- **`spawn()` unimplemented.** Epic would need to implement the fork-helper pattern itself.

**Verdict: Strong reference implementation, not a direct dependency.** Yule-sandbox confirms the approach is sound and shows the exact crate versions and API patterns. Epic should implement its own sandbox module following the same layered pattern, adapted for per-task-phase policies.

### Consolidated Recommendation

| Platform | Mechanism | Source |
|---|---|---|
| Linux | Landlock (filesystem) + seccomp-BPF (syscall filter) + rlimit (memory) | Direct impl, informed by yule-sandbox |
| macOS | Seatbelt FFI (`sandbox_init`) + rlimit (memory) | Direct impl, informed by yule-sandbox |
| Windows | AppContainer/LPAC (filesystem + network + process isolation) + Job objects (resource limits) | `rappct` crate |

**Pattern:** Fork a short-lived helper process per bash tool invocation. The helper applies the sandbox to itself (Landlock/Seatbelt/AppContainer), then `exec`s the target command. The orchestrator communicates via pipes and retains full access.

**Advantages over Frida:**
- Kernel-enforced on all platforms — no TOCTOU, no hook latency, no race conditions.
- No runtime injection. No JS scripting layer. No Frida runtime to bundle.
- No tokio thread pool interaction concerns (sandboxing applies to child, not orchestrator).
- Simpler dependency chain. Well-understood OS primitives.

**Disadvantages vs Frida:**
- Three platform-specific implementations (but yule-sandbox shows each is 50-150 lines).
- Landlock requires Linux 5.13+. Older kernels fall back to seccomp-only (weaker filesystem control).
- macOS Seatbelt (`sandbox_init`) is deprecated by Apple. Still functional and used by major apps, but no new features.
- In-process tool operations (epic's own `read_file`/`write_file`/`edit_file`) are not covered by child-process sandboxing. Existing `safe_path()` + `ToolGrant` enforcement continues to cover these.

**Status:** Research complete. Recommendation: adopt the OS-native combined strategy. Next step is design of the per-phase policy model and fork-helper architecture.
