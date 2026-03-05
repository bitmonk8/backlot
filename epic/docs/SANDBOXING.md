# Sandboxing

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

1. **Startup detection** — Best-effort check for container/VM environment. If not detected, emit a prominent warning recommending the user run epic inside an isolated environment.
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
