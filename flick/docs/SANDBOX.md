# Flick — Sandboxing Future Work

## Current State

The `[[resources]]` configuration provides path-level access control for the builtin file tools (`read_file`, `write_file`, `list_directory`). It is a voluntary check inside Flick's process, not an OS-level sandbox. The following bypass it entirely:

- `shell_exec = true` — arbitrary shell commands with full process-user access
- `command`-mode custom tools — model-controlled parameter substitution into shell templates

No OS-level isolation exists. A model can read, write, or delete anything the process user can access.

## Goal

Enforce operator-declared tool policy at the OS level so that accidental or unintended model-driven actions are caught and blocked, regardless of which tools are enabled.

This is **intent modeling**, not adversarial containment. The threat is not a hostile actor attempting to escape a sandbox — it is a model calling a tool outside the scope the operator intended. The analogy is Claude Code's permission system: once shell access is granted, Claude Code can in principle do anything. For genuine security, the entire agent must run inside an appropriately configured VM or container. OS-level per-tool sandboxing inside Flick serves a different purpose: it gives operators a clear, auditable way to declare what each tool is allowed to do, and provides best-effort enforcement of that intent.

Consequences of this framing:
- Bypass resistance (e.g., raw syscall invocation) is not a design requirement. Non-adversarial tools don't issue direct syscalls.
- Enforcement does not need to be hermetic. Best-effort is acceptable.
- Read access restriction is as important as write access restriction under the intent-modeling framing. Controlling what a model can read directly influences its behaviour; a model that reads outside its declared scope is operating outside operator intent, regardless of whether it writes anything.
- Platform parity is desirable but not a hard requirement. A Linux gap is more tolerable than a Windows or macOS gap.

**Requirements:**
- Per-subprocess overhead must be minimal (operator-configured tools run frequently)
- Enforcement must reflect `ResourceConfig` (the declared policy) as closely as possible
- Graceful degradation when a mechanism is unavailable (warn, don't fail)

---

## Research Summary (March 2026)

Four approaches were researched. Key findings below.

---

## Approach B — Native OS Primitives (the "Chromium model")

Implement platform-specific sandboxing in Flick directly, using OS APIs, behind a unified `Sandbox` trait:

```
Sandbox::wrap_command(&self, cmd: &mut Command, policy: &SandboxPolicy)
```

Each platform has its own backend behind `#[cfg(target_os)]`. Flick's existing `ResourceConfig` (path + access level) maps naturally to all three platform APIs.

### Linux — Landlock + seccomp

**Landlock LSM** (kernel 5.13+) restricts filesystem access at the kernel object level. Applied in the child process after `fork()`, before `exec()`, via a `pre_exec` hook.

| ABI | Kernel | Adds |
|-----|--------|------|
| V1 | 5.13 | Filesystem read/write/execute/create/remove |
| V2 | 5.19 | Cross-directory rename/link |
| V3 | 6.2 | Truncate |
| V4 | 6.7 | TCP bind/connect network restriction |
| V5 | 6.10 | Device ioctl |
| V6 | 6.12 | Unix socket + signal isolation |

ABI v1 covers Ubuntu 22.04 LTS, RHEL 9, Fedora 38+, Debian 12. The `landlock` crate (v0.4.4, maintained by the upstream Landlock project) supports graceful degradation when the kernel ABI is lower than requested.

**seccomp-bpf** blocks dangerous syscalls (ptrace, mount, reboot, kexec_load) as defense-in-depth. `seccompiler` crate (v0.5.0, rust-vmm/Firecracker lineage) compiles BPF filters in pure Rust without C dependencies. Seccomp cannot filter by filename, only by syscall number — so it complements Landlock rather than replacing it.

**Overhead:** <100 µs setup per subprocess (3 landlock syscalls + 1 prctl). Runtime overhead: <0.09% on realistic workloads. Near-zero.

**New crate deps:** `landlock = "0.4"` + `seccompiler = "0.5"` (Linux only). Both encapsulate unsafe internally; Flick's `unsafe_code = "deny"` lint is unaffected.

**Assessment under intent-modeling:** Excellent fit. Low complexity, near-zero overhead, directly enforces `ResourceConfig` paths in the kernel. Graceful degradation on older kernels is built in. This is the cleanest implementation in the codebase.

### macOS — Seatbelt (sandbox-exec / sandbox_init)

**sandbox-exec** is marked deprecated since 2016 but is used in production by Chrome, Claude Code, Cursor, and OpenAI Codex as of 2026. Apple uses Seatbelt internally for all system software; the kernel subsystem is not going away. The CLI wrapper (`/usr/bin/sandbox-exec`) is the risk, not the mechanism.

Two invocation paths:
1. Prefix the command: `sandbox-exec -f <profile.sb> -- <command>`. Simplest, no FFI needed.
2. Call `sandbox_init()` in the child after `fork()` before `exec()`. More control, requires FFI or a wrapper crate.

Profile language (SBPL, Scheme-dialect):

```scheme
(version 1)
(deny default)
(allow file-read* (subpath "/usr/lib"))
(allow file-write* (literal "/workspace"))
(deny network*)
```

Flick generates the profile string from `ResourceConfig` at runtime.

**Overhead:** <1 ms per subprocess (profile parse + kernel rule install). Runtime checks are in-kernel, negligible.

**New crate deps:** None required (invoke `sandbox-exec` via `Command`, or use libc FFI for `sandbox_init`).

**Assessment under intent-modeling:** Excellent fit. Well-proven, ships with every macOS, no external deps, and profile generation from `ResourceConfig` is straightforward. Deprecation risk is low given Chrome and Claude Code depend on it.

### Windows — Restricted Token (primary) / AppContainer (optional)

Under the intent-modeling framing, AppContainer's ACL management overhead is disproportionate. The primary mechanism is:

**`CreateRestrictedToken` + Low Integrity Level:** Removes write access to all Medium-integrity objects (default for user files). No admin requirements, no ACL management, no cleanup after subprocess exits. Enforcement is write-only — read access is not restricted. This is a significant gap: under the intent-modeling framing, read restriction is as important as write restriction.

**Job Object:** Kill-on-close semantics and process resource limits. Adds ~1 ms. Applies regardless of integrity level.

**AppContainer:** Full path-granular read+write restriction. Given that read restriction is as important as write restriction under the intent-modeling framing, AppContainer is the mechanism needed to fully meet the goal on Windows — not merely a strict-mode option. Cost: requires modifying filesystem DACLs to grant access, reverting after the call, and handling crash/cleanup. Implementation complexity is high, but this is the inherent cost of proper enforcement on this platform.

**Overhead:** <2 ms (restricted token + job object).

**New crate deps:** Extended `windows` crate features: `Win32_Security` (restricted token). The `windows` crate is already a Flick dependency.

**Assessment under intent-modeling:** Restricted token alone does not meet the intent-modeling goal — it enforces write intent but not read intent. AppContainer is required for a complete implementation. The restricted token + job object layer is a useful partial step (fast to ship, zero admin requirements) but should be treated as a temporary baseline, not as the permanent `basic` mode. AppContainer should follow immediately in the implementation plan, not be deferred.

### Cross-platform summary

| | Windows | macOS | Linux |
|---|---|---|---|
| Mechanism | Restricted Token + Job Object | Seatbelt (sandbox-exec) | Landlock + seccomp |
| Filesystem write restriction | Integrity-level (all user files) | Kernel-enforced, path-granular | Kernel-enforced, path-granular |
| Filesystem read restriction | None (AppContainer opt-in only) | Kernel-enforced, path-granular | Kernel-enforced, path-granular |
| Network restriction | Capability-based (AppContainer only) | All-or-nothing | ABI v4+ (kernel 6.7) |
| Overhead per call | <2 ms | <1 ms | <100 µs |
| External dependencies | None (windows crate) | None (sandbox-exec ships with OS) | None |
| Implementation complexity | Low (restricted token) / High (AppContainer) | Medium | Low |
| API stability | Stable Win32 | Deprecated-but-stable (10+ years) | Stable kernel ABI |

**Configurable levels:**

| Level | Windows | macOS | Linux |
|-------|---------|-------|-------|
| `off` | No sandbox | No sandbox | No sandbox |
| `basic` | Restricted Token + Job Object (write-only; partial) | Seatbelt deny-network + path restrict | Landlock filesystem only |
| `strict` | AppContainer + Job Object (read+write, path-granular; ACL management required) | Seatbelt full deny-default | Landlock + seccomp allowlist |

**Implementation order:** Windows first (highest priority platform; restricted token as temporary baseline, AppContainer following immediately), macOS second (well-proven, medium complexity), Linux last (lowest complexity, best tooling).

---

## Approach C — Third-Party Wrapper Prefix

Add a `[sandbox] wrapper = [...]` config array. Flick prefixes every tool subprocess invocation with the operator-supplied command. E.g., `["bwrap", "--ro-bind", "/usr", "/usr", "--bind", "{cwd}", "{cwd}"]`.

| Platform | Tool | Notes |
|----------|------|-------|
| Linux | **bubblewrap (bwrap)** | In standard repos (Ubuntu, Fedora, Arch). Rootless on modern kernels. ~8 ms overhead. Used by Flatpak, Claude Code, Cursor. |
| Linux | Firejail | Alternative. SUID required; not in Fedora official repos; multiple historical CVEs. Weaker choice. |
| macOS | **sandbox-exec** | Built into every macOS. ~1–5 ms overhead. Used by Claude Code, Cursor, Chromium. |
| Windows | Sandboxie-Plus | Requires kernel driver (SbieDrv.sys) and admin to install. ~50–200 ms overhead. Active CVEs. |

**Linux detail (bwrap):**
- Explicit bind-mount model: sandbox starts with empty tmpfs root; only explicitly bound paths are visible.
- No constraint on which paths can be bound (unlike Firejail's limited top-directory whitelist).
- Overhead measured at ~8 ms (`bwrap ls /`). No daemon.
- Already used by Anthropic's own sandbox-runtime.

**macOS detail (sandbox-exec):**
- Present on macOS 13/14/15. Confirmed functional on Sequoia (15.x).
- Flick generates a Seatbelt profile from `ResourceConfig`.
- `sandbox-exec -f <profile> -- <command>` needs zero FFI.

**Windows gap:**
- No equivalent of bwrap/sandbox-exec exists natively on Windows.
- Sandboxie-Plus works as a CLI prefix but requires kernel driver (admin install), has active security vulnerabilities, and adds 50–200 ms overhead.
- Windows Sandbox (built-in) boots a full Hyper-V VM (10–30 seconds). Unusable.

**Assessment under intent-modeling:** Approach C is highly aligned with the intent-modeling philosophy. The operator writes the wrapper command explicitly — the policy is visible, operator-owned, and requires zero security code in Flick. Linux and macOS are well-served. The Windows gap is a significant limitation given Windows is the primary platform: no viable wrapper exists without kernel driver installation, admin privileges, and active security vulnerabilities (Sandboxie-Plus). This approach cannot serve as a primary mechanism on Windows; it is supplementary for macOS and Linux operators.

This approach also decouples Flick's release cycle from sandbox policy evolution. An operator can update their bwrap arguments without a Flick update.

**Detection:** At startup, check for the first element of `wrapper` in PATH. Emit a warning if absent rather than failing.

---

## Approach D — Containerization (Docker / Podman)

Flick starts one container at session start; all tool calls run inside it via `docker exec`; Flick stops it at session end.

### Performance (measured)

| | Linux native | macOS Docker Desktop | Windows Docker Desktop (WSL2) |
|-|---|---|---|
| `docker exec` overhead | ~100 ms | ~150–300 ms | ~150–300 ms |
| Bind-mount I/O penalty | Near zero | ~3× (VirtioFS) | Very slow if files on NTFS |

Docker Desktop imposes a 2.7× startup penalty vs native Linux (VM round-trip). The container startup cost is amortised across all tool calls in the session.

**Verdict:** Viable as an opt-in power-user mode. 100–300 ms per `docker exec` call is acceptable. Implementation is moderate complexity (start/exec/stop lifecycle, crash handling).

### Isolation model

Docker containers isolate via PID namespace, network namespace, mount namespace, capability dropping (14 of 41 by default), seccomp (blocks ~44 syscalls), and cgroups. This is the strongest isolation of any approach researched.

Recommended hardened config:
```
--cap-drop ALL
--security-opt no-new-privileges
--read-only
--tmpfs /tmp:rw,noexec,nosuid,size=256m
--network none
--memory 512m
--pids-limit 256
-v {cwd}:/workspace:rw
```

### Podman as alternative

Rootless by default. Daemonless (fork-exec). ~95% CLI-compatible with Docker. Supports all three platforms (Linux native, macOS via podman machine, Windows via podman machine on Hyper-V). Container startup slightly faster than Docker. No persistent daemon memory overhead. On Windows, like Docker Desktop, containers run inside a Linux VM — native Windows process isolation is not provided.

Flick should detect any compatible runtime: `docker`, `podman`, `nerdctl`, tried in order.

### Practical gaps

- **macOS/Windows file I/O:** VirtioFS helps on macOS but I/O-heavy tools will still see overhead.
- **Path translation:** On Windows, host paths must map to container paths (`/workspace`). Working directory must be under a Docker-shared path.
- **Image management:** Flick would need a default sandbox image and a config option to override it. Image pull on first use adds latency.
- **Cross-call state:** The persistent container accumulates state (files, processes) across tool calls. A corrupted temp file or hung background process in the container affects subsequent calls.

**Assessment under intent-modeling:** The right tool for operators who want strong isolation — it provides full VM-equivalent containment without requiring Flick to run inside a VM. The 100–300 ms overhead is the cost of that guarantee. Position as an opt-in mode (`sandbox.mode = "container"`), not the default.

Note: container sandboxing is the bridge between per-tool enforcement (Flick's scope) and full VM isolation (outside Flick's scope). Operators running this mode get close to VM-level isolation without the VM deployment overhead.

---

## Approach E — Alternatives Researched

### Landlock (Linux-only deeper dive)

- `extrasafe` crate (0.x): wraps both Landlock and seccomp in a unified deny-by-default API. ~10k downloads. Linux-only.
- Landlock cannot restrict network on kernels <6.7. On kernels 5.13–6.6, combine with seccomp to block socket syscalls explicitly.
- Graceful degradation: the `landlock` crate's `best_effort()` mode applies the highest supported ABI silently, so Flick works correctly on all kernels ≥5.13.

### pledge/unveil (Linux ports)

Justine Tunney's pledge implementation works via seccomp-BPF on Linux but only with her Cosmopolitan libc. glibc's internal syscall usage is unstable; pledge filters valid for glibc 2.35 may crash 2.36. Not viable for Flick.

### Windows restricted tokens (no AppContainer)

`CreateRestrictedToken` + Low Integrity Level: provides write restriction on all Medium-integrity objects (default for user files). No read restriction, no path granularity. Zero admin requirements. Elevated to primary Windows mechanism under intent-modeling framing (see Approach B Windows section).

### bubblewrap (Linux-only)

~8 ms overhead. Namespace-based (separate mount tree, PID, network). Stronger than Landlock alone (complete filesystem virtualization). External binary dependency. Used by Flatpak, Claude Code, Cursor. Best used on Linux when network namespace isolation is needed (Landlock v4 requires kernel 6.7 for network restriction). Available as an Approach C wrapper.

---

## Comparison Matrix

| Approach | Windows | macOS | Linux | Overhead | External dep | Complexity | Notes |
|----------|---------|-------|-------|----------|--------------|------------|-------|
| **B: Native primitives** | Partial | Yes | Yes | <2 ms | None | Low–Med | Best built-in option. Windows `basic` is write-only (partial); full intent-modeling on Windows requires AppContainer (`strict`). |
| **C: Wrapper prefix** | No viable wrapper | Yes | Yes | 1–8 ms | bwrap (Linux) | Low | Operator-owned policy. Supplementary for macOS/Linux only; not viable as primary on Windows. |
| **D: docker exec** | Yes (WSL2) | Yes | Yes | 100–300 ms | Docker/Podman | Medium | Opt-in power-user mode. Strongest isolation. |

---

## Recommendation

*Section cleared.*

---

## Open Questions Before Implementation

*Section cleared.*
