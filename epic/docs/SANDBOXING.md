# Sandboxing

> **Operational correctness sandboxing is implemented via [lot](https://github.com/bitmonk8/lot)** — a standalone cross-platform process sandboxing library (Seatbelt on macOS, AppContainer on Windows, namespaces + seccomp + cgroups v2 on Linux). The nu tool spawns a persistent `nu --mcp` process inside a lot sandbox with per-phase policies (write access only during Execute/Fix phases). One nu MCP session per agent call. Sandbox is mandatory — if lot cannot establish a sandbox, the tool call fails with an error. See [LOT_SPEC.md](LOT_SPEC.md) for the lot design spec.

Epic sandboxing addresses two distinct concerns with fundamentally different solutions.

## Concern 1: Security Isolation

### Problem

LLM agents execute arbitrary shell commands and file operations. A compromised or misbehaving agent could read secrets, exfiltrate data, modify system files, or attack the network. No amount of in-process checking can fully prevent a determined or unexpected escape.

### Approach: User-Managed VM/Container

Epic does **not** attempt OS-level security sandboxing (see Concern 2 for operational correctness sandboxing via lot). The only robust security boundary is running epic itself inside an appropriately configured virtual environment:

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

None of these are foolproof. Detection is best-effort — a false positive means the warning shows unnecessarily, which is acceptable.

### Non-Goals

- Epic will not refuse to run outside a container — only warn.

---

## Concern 2: Correct Epic Operation

### Problem

Each epic operation (assessment, decomposition, leaf execution, verification, etc.) has a defined contract for what it should access. `ToolGrant` bitflags and `safe_path()` validation provide prompt-level and path-level enforcement, but shell commands bypass both — an agent with NU grant has effectively unrestricted access within the sandbox.

### Solution: lot Process Sandboxing

The nu tool spawns a persistent `nu --mcp` process inside a [lot](https://github.com/bitmonk8/lot) sandbox (`build_nu_sandbox_policy` in `src/agent/nu_session.rs`). Per-phase policies control access:

| Phase | Project Root | Temp Dirs | Network |
|---|---|---|---|
| Assess / Decompose / Verify | Read-only | Writable | Allowed |
| Execute / Fix | Writable | Writable | Allowed |

Platform mechanisms:
- **Linux:** namespaces + seccomp-BPF + rlimit
- **macOS:** Seatbelt (`sandbox_init`) + rlimit
- **Windows:** AppContainer + Job objects

Sandbox is mandatory: if sandbox setup fails (permissions, unsupported kernel), the tool call returns an error with lot's diagnostic message. There is no unsandboxed fallback.

### Existing Enforcement (Retained)

Lot sandboxing is an additional layer. Existing mechanisms remain:

- **`ToolGrant` bitflags** — Controls which tools are offered to agents per phase.
- **`safe_path()` containment** — Validates paths in epic's own tool implementations.
- **`required_grant()` check in `execute_tool()`** — Rejects tool calls that don't match the current grant.
