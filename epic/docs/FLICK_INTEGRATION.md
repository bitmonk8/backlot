# Flick Integration

## Status

**Placeholder.** Flick selected as agent runtime replacement for ZeroClaw. Integration details TBD.

## Role

Flick serves as the **agent runtime** — an external executable that Epic invokes as a subprocess for each agent call. Epic is the **orchestrator** — it decides what work to do, which model to use, which tools to grant, and what system prompt to send. Flick executes individual agent sessions; Epic manages the recursive task tree.

## Integration Mode: Subprocess

Epic invokes Flick as an external executable. No Rust crate dependency. Communication via stdin/stdout/stderr and process exit codes.

Benefits over the previous ZeroClaw library approach:
- No transitive dependency tree (~771 packages eliminated, down to ~104)
- No git submodule to maintain
- No fork maintenance burden
- Clean process boundary — crashes in Flick don't crash Epic
- Flick can be updated independently of Epic's Rust toolchain

## Repository

- [github.com/bitmonk8/flick](https://github.com/bitmonk8/flick)

## Integration Details

TBD. Key areas to define:
- CLI invocation pattern (model selection, tool grants, system prompt)
- Structured output capture (JSON on stdout)
- Error handling (exit codes, stderr)
- Timeout and process management
- Tool scoping mechanism

## Previous Runtime: ZeroClaw

ZeroClaw (library dependency via forked submodule) was replaced due to:
- Provenance concerns (star farming, 12-day-old project, crypto fraud ecosystem)
- Heavy transitive dependency tree (~771 packages)
- Fork maintenance burden (hardening patches, upstream tracking)
- Intermittent compiler ICE with `tracing` 0.1.44 on rustc 1.93.1

ZeroClaw audit documentation was removed along with the dependency.
