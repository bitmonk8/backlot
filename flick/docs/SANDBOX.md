# Flick — Sandboxing

## Current State

The `[[resources]]` configuration provides path-level access control for the builtin file tools (`read_file`, `write_file`, `list_directory`). It is a voluntary check inside Flick's process, not an OS-level sandbox. The following bypass it entirely:

- `shell_exec = true` — arbitrary shell commands with full process-user access
- `command`-mode custom tools — model-controlled parameter substitution into shell templates

No OS-level isolation exists. A model can read, write, or delete anything the process user can access.

## Goal

Enforce operator-declared tool policy at the OS level so that accidental or unintended model-driven actions are caught and blocked, regardless of which tools are enabled.

This is **intent modeling**, not adversarial containment. The threat is not a hostile actor attempting to escape a sandbox — it is a model calling a tool outside the scope the operator intended. OS-level per-tool sandboxing inside Flick gives operators a clear, auditable way to declare what each tool is allowed to do, and provides best-effort enforcement of that intent. For genuine security, the entire agent must run inside an appropriately configured VM or container.

Consequences of this framing:
- Bypass resistance (e.g., raw syscall invocation) is not a design requirement.
- Enforcement does not need to be hermetic. Best-effort is acceptable.
- Read access restriction is as important as write access restriction.
- Platform parity is desirable but not a hard requirement.

**Requirements:**
- Per-subprocess overhead must be minimal (operator-configured tools run frequently)
- Enforcement must reflect `ResourceConfig` (the declared policy) as closely as possible
- Fail fast when sandbox is configured but unavailable (misconfiguration must not silently degrade to unsandboxed execution)

---

## Phase 1 — Generic Wrapper Prefix

Flick prefixes every tool subprocess invocation with an operator-supplied command template. Flick has zero knowledge of any specific sandbox tool — the operator owns the policy entirely. This decouples Flick's release cycle from sandbox policy evolution.

Known-compatible tools: bubblewrap (Linux), firejail (Linux), sandbox-exec (macOS), Sandboxie-Plus (Windows). Any tool that accepts a command as trailing arguments works.

### Configuration

```toml
[sandbox]
# Base wrapper command. Prepended to every subprocess invocation.
wrapper = ["bwrap", "--die-with-parent", "--new-session"]

# Appended once per [[resources]] entry with access = "read".
# {path} expands to the resource's absolute path.
read_args = ["--ro-bind", "{path}", "{path}"]

# Appended once per [[resources]] entry with access = "read_write".
read_write_args = ["--bind", "{path}", "{path}"]

# Appended once at the end, before the target command.
suffix = ["--"]

# Optional: generated policy file for tools that take a file-based policy
# (e.g., sandbox-exec). Written once at startup.
# {pid} expands to Flick's process ID (for temp file uniqueness).
policy_file = "/tmp/flick-sandbox-{pid}.sb"

# Template for the policy file content.
# {read_rules} expands to one policy_read_rule per read resource.
# {read_write_rules} expands to one policy_read_write_rule per read_write resource.
policy_template = """
(version 1)
(deny default)
{read_rules}
{read_write_rules}
"""

# Per-resource line templates for the policy file.
policy_read_rule = "(allow file-read* (subpath \"{path}\"))"
policy_read_write_rule = "(allow file-read* file-write* (subpath \"{path}\"))"
```

### Placeholders

| Placeholder | Expands to | Available in |
|-------------|-----------|--------------|
| `{cwd}` | Working directory (absolute) | all fields |
| `{path}` | Resource absolute path | `read_args`, `read_write_args`, `policy_read_rule`, `policy_read_write_rule` |
| `{policy_file}` | Path to generated policy file | `wrapper`, `suffix` |
| `{pid}` | Flick's process ID | `policy_file` |

### Command assembly

Final command for each subprocess:

```
[wrapper...] [read_args per read resource...] [read_write_args per rw resource...] [suffix...] <original command>
```

- If `[sandbox]` is absent → no wrapping
- If `read_args` / `read_write_args` are absent → no per-resource expansion (tools like Sandboxie-Plus manage policy externally)
- If `policy_file` + `policy_template` are set → write policy file once at startup

### Startup behavior

1. If `wrapper` is set, check that `wrapper[0]` exists in PATH (or is an absolute path that exists)
2. If absent → exit with error. Sandbox was configured; running without it is not acceptable.
3. If `policy_file` is set → expand template from `[[resources]]`, write file
4. Proceed with agent loop

### Platform examples

**bubblewrap (Linux)** — inline bind-mount policy:

```toml
[sandbox]
wrapper = [
    "bwrap",
    "--die-with-parent",
    "--new-session",
    "--unshare-pid",
    "--unshare-net",
    "--proc", "/proc",
    "--dev", "/dev",
    "--tmpfs", "/tmp",
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/lib", "/lib",
    "--ro-bind", "/lib64", "/lib64",
    "--ro-bind", "/bin", "/bin",
    "--ro-bind", "/etc/resolv.conf", "/etc/resolv.conf",
    "--chdir", "{cwd}",
]
read_args = ["--ro-bind", "{path}", "{path}"]
read_write_args = ["--bind", "{path}", "{path}"]
suffix = ["--"]
```

**firejail (Linux)** — inline whitelist policy:

```toml
[sandbox]
wrapper = ["firejail", "--noprofile", "--net=none", "--nosound", "--no3d"]
read_args = ["--whitelist={path}", "--read-only={path}"]
read_write_args = ["--whitelist={path}"]
suffix = ["--"]
```

**sandbox-exec (macOS)** — file-based Seatbelt profile:

```toml
[sandbox]
wrapper = ["sandbox-exec", "-f", "{policy_file}"]
suffix = ["--"]
policy_file = "/tmp/flick-sandbox-{pid}.sb"
policy_template = """
(version 1)
(deny default)
(allow process*)
(allow sysctl-read)
(allow mach-lookup)
(allow file-read*
    (subpath "/usr/lib")
    (subpath "/usr/share")
    (subpath "/private/var")
    (subpath "/dev"))
{read_rules}
{read_write_rules}
"""
policy_read_rule = "(allow file-read* (subpath \"{path}\"))"
policy_read_write_rule = "(allow file-read* file-write* (subpath \"{path}\"))"
```

**Sandboxie-Plus (Windows)** — externally managed policy:

```toml
[sandbox]
wrapper = ["C:\\Program Files\\Sandboxie-Plus\\Start.exe", "/box:FlickBox", "/silent", "/wait"]
# Policy is managed in Sandboxie.ini, not via CLI args.
# No read_args/read_write_args needed.
```

### Implementation scope

Flick code changes:
1. `config.rs` — add `SandboxConfig` struct, parse `[sandbox]` section
2. `tool.rs` / `CommandRunner` — expand placeholders and prepend wrapper to subprocess commands
3. Startup — PATH check for wrapper binary, policy file generation

All logic is mechanical string expansion. No tool-specific knowledge in Flick.

---

## Phase 2 — Native OS Primitives

Implement platform-specific sandboxing in Flick directly, using OS APIs, behind a unified `Sandbox` trait. Each platform behind `#[cfg(target_os)]`. Flick's existing `ResourceConfig` maps to all three platform APIs.

| Platform | Mechanism | Read restriction | Write restriction | Overhead | New deps |
|----------|-----------|-----------------|-------------------|----------|----------|
| Linux | Landlock + seccomp | Path-granular, kernel-enforced | Path-granular, kernel-enforced | <100 µs | `landlock`, `seccompiler` |
| macOS | Seatbelt (`sandbox_init` FFI) | Path-granular, kernel-enforced | Path-granular, kernel-enforced | <1 ms | None |
| Windows | Restricted Token + Job Object | None (accepted gap) | Integrity-level (all user files) | <2 ms | `windows` crate features |

Implementation order: Linux, Windows, macOS.

---

## Phase 3 — Containerization (Linux only)

Opt-in mode (`sandbox.mode = "container"`). Flick starts one container at session start; tool calls run via `docker exec`; container stops at session end.

- Detect compatible runtimes: `docker`, `podman`, `nerdctl` (tried in order)
- Linux only — macOS/Windows VM round-trip adds 150+ ms overhead and I/O penalty
- 100–300 ms overhead per tool call (amortised container startup)

---

## Decisions

| Decision | Rationale |
|----------|-----------|
| Generic wrapper config, not hardcoded tool support | Operator owns policy. Flick does mechanical string expansion only. Supports any wrapper tool without Flick changes. |
| Template-based resource expansion | Bridges `[[resources]]` to wrapper CLI args and policy files without tool-specific logic. |
| `policy_file` + `policy_template` for file-based tools | sandbox-exec and similar tools require a policy file, not CLI flags. Template generation covers this without special-casing. |
| Three-phase plan (wrapper → native → container) | Wrapper first (lowest effort, immediate value), native primitives (best overhead), containers (strongest isolation, opt-in). |
| Windows native sandbox: write-only (accepted gap) | Restricted token + job object. No read restriction. AppContainer rejected (high complexity). |
| Containers Linux-only | macOS/Windows VM round-trip overhead and I/O penalty make container sandboxing unattractive on those platforms. |
