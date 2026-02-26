# ZeroClaw Security Audit — Summary

**Date:** 2026-02-25
**Codebase:** C:\UnitySrc\ZeroClaw (v0.1.7, ~150K lines Rust)
**Auditors:** 16 Claude Opus agents, each reviewing a distinct module
**Lines reviewed:** ~130K+ (all production source code)

## Overall Verdict: CONDITIONAL PASS

No deliberate backdoors, data exfiltration mechanisms, or covert channels were found. The codebase demonstrates competent security engineering with defense-in-depth. All outbound network calls go to documented, legitimate API endpoints. All conditions are addressable without architectural changes.

## Unit Verdicts

| Unit | Scope | Lines | Verdict |
|------|-------|-------|---------|
| AU-01 | LLM Providers | 19,825 | CONDITIONAL PASS |
| AU-02a | Channels: core, Telegram, Discord, Lark | 15,342 | CONDITIONAL PASS |
| AU-02b | Channels: WhatsApp, Matrix, Email, iMessage, Signal, IRC | 8,221 | PASS |
| AU-02c | Channels: 12 remaining | 5,805 | PASS |
| AU-03 | HTTP Gateway + web frontend | 9,576 | CONDITIONAL PASS |
| AU-04 | Agent Tools (network) | 8,713 | CONDITIONAL PASS |
| AU-05 | Observability & Telemetry | 2,668 | PASS |
| AU-06 | Tunnel & WebSocket | 1,084 | PASS |
| AU-07 | Auth & Secrets | 4,071 | PASS |
| AU-08 | Agent Tools (file I/O) | 10,374 | CONDITIONAL PASS |
| AU-09 | Memory & Persistence | 7,926 | PASS |
| AU-10 | Security Policy | 7,726 | CONDITIONAL PASS |
| AU-11 | Agent Loop & Decision Logic | 9,067 | CONDITIONAL PASS |
| AU-12 | Config & Initialization | 16,957 | PASS |
| AU-13 | Dependencies (Cargo.toml/Cargo.lock) | 8,589 | CONDITIONAL PASS |
| AU-14 | Remaining Modules (17 groups) | 18,997 | CONDITIONAL PASS |

**PASS: 7 units | CONDITIONAL PASS: 9 units | FAIL: 0 units**

## Findings by Severity

### HIGH (2) — Supply chain / provenance

| ID | Unit | Finding | Mitigation |
|----|------|---------|------------|
| AU-13-F1 | Dependencies | `wa-rs` crate family: 10+ crates, all 8 days old, unknown publisher (`fabiocantone`/`homunbot`), includes proc-macro (`wa-rs-derive`) and build-time codegen (`wa-rs-proto`). Custom Signal Protocol crypto from unknown author. | `whatsapp-web` feature disabled by default. Do not enable without auditing wa-rs source. |
| AU-13-F2 | Dependencies | Overall project provenance: 12-day-old repo, ~19K stars (star farming), multiple inconsistent identities (`theonlyhennygod`, `fabiocantone`, `homunbot`). | Contextual risk. Code quality is high, but maintenance/trust trajectory is uncertain. |

### MEDIUM (5) — Security gaps (not backdoors)

| ID | Unit | Finding | Mitigation |
|----|------|---------|------------|
| AU-11-F1 | Agent Loop | GLM compatibility fallback auto-converts bare URLs in LLM responses to `curl` shell commands. Prompt injection surface for LLM-mediated outbound requests. | Gate behind security policy or remove GLM URL-to-curl fallback. |
| AU-03-F1 | Gateway | WATI webhook endpoint has zero authentication. Any network-reachable attacker can forge messages. | Implement signature verification or rate limiting for WATI webhook. |
| AU-02a-F1 | Channels | Lark HTTP webhook handler does not verify event signatures. Only affects `receive_mode = "webhook"` (WebSocket mode is default and unaffected). | Implement `X-Lark-Signature` verification in HTTP webhook mode. |
| AU-10-F7 | Security | Firejail sandbox backend lacks `--net=none`, allowing network access from sandboxed processes. Docker and Bubblewrap backends isolate correctly. | Add `--net=none` and `--seccomp` to Firejail wrap command. |
| AU-04-F1 | Network Tools | `web_search_tool` is the only network tool that bypasses `SecurityPolicy.can_act()` / `record_action()`. No rate limiting, no autonomy level enforcement. | Add SecurityPolicy reference and enforce checks. |

### LOW (8) — Defense-in-depth gaps

| ID | Unit | Finding |
|----|------|---------|
| AU-04-F5 / AU-08-F5 | Network Tools / File I/O | `http_request` lacks DNS rebinding protection (unlike `web_fetch` which has it). |
| AU-08-F2 | File I/O | `content_search` (rg/grep) follows symlinks by default; could leak file contents outside sandbox. Fix: add `--no-follow`. |
| AU-03-F3 | Gateway | Webhook signature verification is conditional — skipped if operator doesn't configure the secret. No startup warning. |
| AU-01-F1 | Providers | Copilot provider uses VS Code's OAuth client ID to impersonate VS Code. ToS concern, not a security vulnerability. |
| AU-14-F1 | Remaining | `open_skills` feature (disabled by default) clones from hardcoded third-party repo `besoeasy/open-skills`. |
| AU-14-F2 | Remaining | `skillforge` feature (disabled by default) makes GitHub API requests revealing search intent. |
| AU-02a-F2 | Channels | Lark webhook challenge verification uses `map_or(true, ...)` — missing token passes validation. |
| AU-10-F8 | Security | Landlock sandbox allows `/tmp` write access. |

## Key Positive Findings

The audit confirmed several strong security properties:

1. **No phone-home or telemetry.** No startup beacons, version checks, analytics, or author-controlled infrastructure in defaults.
2. **No hardcoded exfiltration endpoints.** Every outbound URL traces to a documented, legitimate API.
3. **Credentials correctly scoped.** Each channel/provider token flows only to its intended API. Multi-layer scrubbing (`scrub_credentials`, `sanitize_api_error`, `LeakDetector`) prevents credential leakage in outputs.
4. **Secrets encrypted at rest.** ChaCha20-Poly1305 AEAD with CSPRNG keys, 0600 file permissions.
5. **Deny-by-default security model.** Command allowlists, path sandboxing (6 layers), rate limiting, autonomy levels.
6. **Config is local-only.** TOML from disk, no remote config fetching.
7. **Minimal unsafe code.** Only 2 instances (both safe Unix syscalls).
8. **No obfuscation.** No suspicious base64 chains, XOR operations, steganographic data, or encoded payloads.
9. **Web frontend is clean.** No external URLs, no eval, no tracking scripts, no third-party analytics.
10. **Cargo.lock committed.** All mainstream dependencies pinned to exact versions with checksums.

## Relevance to Epic

Epic's planned use of ZeroClaw is limited to: `AgentBuilder` API, Anthropic provider, Tool trait, SecurityPolicy. This subset falls within AU-01 (providers), AU-10 (security), AU-11 (agent loop), and partially AU-08 (file tools).

**For Epic's use case specifically:**
- The Anthropic provider (AU-01) is clean — payloads contain only documented API fields, sent only to `api.anthropic.com`.
- The security model (AU-10) is sound and deny-by-default.
- The agent loop (AU-11) is clean except for the GLM fallback (which Epic would not use since it targets Anthropic models only).
- The `whatsapp-web` supply chain risk (AU-13-F1) is irrelevant — Epic would not enable that feature.

**Recommendation:** The codebase passes audit for Epic's intended usage. The provenance risk (AU-13-F2) remains a long-term maintenance concern but does not represent a current security threat in the code itself. Pin to an audited commit if adopting.

## Conditions for Full PASS

1. Do not enable `--features whatsapp-web` without auditing `wa-rs` source.
2. If forking ZeroClaw for Epic, remove or gate the GLM URL-to-curl fallback (AU-11-F1).
3. Pin to audited commit hash if depending on ZeroClaw as a crate.
