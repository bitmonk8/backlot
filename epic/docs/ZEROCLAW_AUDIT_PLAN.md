# ZeroClaw Security Audit Plan

## Objective

Determine whether the ZeroClaw codebase (C:\UnitySrc\ZeroClaw, ~150K lines Rust) contains backdoors, data exfiltration mechanisms, or security vulnerabilities that could leak sensitive information to third parties.

**Context:** ZeroClaw is a 12-day-old repo with 160K lines and provenance concerns (star farming, inconsistent author identity, marketing claims not backed by source). This audit is a precondition for deciding whether Epic should depend on ZeroClaw.

## Audit Scope

**In scope:** All Rust source code under `src/`, `Cargo.toml` dependency declarations, build scripts, embedded assets (`web/`).

**Out of scope:** `crates/robot-kit/` (Epic will not use hardware modules), `firmware/` (embedded targets), `python/` (Python bindings). These can be audited separately if needed.

## Threat Model

The primary threats, ordered by severity:

1. **Deliberate exfiltration** — Code that sends user data (API keys, conversation content, file contents, system information) to hardcoded or obfuscated endpoints controlled by the author.
2. **Hidden command-and-control** — Code that fetches instructions from an external server, enabling remote activation of malicious behavior.
3. **Credential harvesting** — Code that captures, stores, or transmits authentication tokens, API keys, or secrets beyond what the documented functionality requires.
4. **Covert channels** — Data embedded in seemingly legitimate outbound traffic (DNS queries, HTTP headers, WebSocket frames, telemetry payloads) that leaks information.
5. **Backdoor access** — Hidden HTTP endpoints, WebSocket listeners, or shell commands that grant unauthorized access.
6. **Supply chain risk** — Dependencies in Cargo.toml that are themselves malicious or unnecessarily broad.
7. **Security model bypass** — Bugs or intentional weaknesses in the SecurityPolicy/sandboxing that allow agents to escape their sandbox.

## Audit Structure

Each audit unit is an independent task suitable for a single Opus agent. Units are ordered by risk priority. Each agent receives:
- The audit unit description below
- Access to the ZeroClaw source at `C:\UnitySrc\ZeroClaw\`
- Instructions to produce a structured findings report

### Audit Unit Template

Each agent should produce a report with:
- **Module**: Which files were reviewed
- **Lines reviewed**: Approximate count
- **Findings**: Each finding categorized as CRITICAL / HIGH / MEDIUM / LOW / INFO
- **Verdict**: PASS (no issues), CONDITIONAL PASS (issues found but mitigable), or FAIL (unacceptable risk)
- **Evidence**: File paths and line numbers for every finding

---

## Tier 1 — Outbound Communication (highest exfiltration risk)

### AU-01: LLM Provider Calls (`src/providers/`, ~20K lines)

**Objective:** Verify that provider modules send only the expected API payloads to the expected endpoints, and nothing else.

**What to look for:**
- All outbound HTTP requests: verify destination URLs are constructed solely from user-provided configuration (API base URL, model name). No hardcoded secondary endpoints.
- Request payloads: verify they contain only the documented API fields (messages, model, tools, etc.). No extra fields that embed system info, file contents, or credentials.
- Response handling: verify responses are parsed and returned to the caller without side-channel transmission.
- Any HTTP requests made outside the main API call flow (telemetry, analytics, version checks, "phone home").
- Obfuscated strings that could be hidden URLs or API keys.

**Files:** All files under `src/providers/` (anthropic, openai, gemini, ollama, bedrock, zhipu, and any others).

### AU-02: Messaging Channels (`src/channels/`, ~29K lines)

**Objective:** Verify that channel implementations only communicate with their documented service (Telegram API, Discord API, etc.) and do not leak conversation content or metadata elsewhere.

**What to look for:**
- All outbound network calls: verify destinations match the documented API for each channel.
- Message content handling: verify user messages, bot responses, and attachments are not copied, logged, or transmitted to any endpoint other than the channel's own API.
- Webhook receivers: verify inbound webhooks are authenticated (signature verification) and do not forward data to undocumented destinations.
- Token/credential handling: verify channel API tokens are used only for their intended channel and not transmitted elsewhere.

**Files:** All files under `src/channels/`. Due to volume (29K lines, 25 implementations), this unit may be split across 2-3 agents by channel grouping if needed.

### AU-03: HTTP Gateway (`src/gateway/`, ~4.5K lines)

**Objective:** Verify the HTTP server exposes only documented endpoints and does not leak data.

**What to look for:**
- Complete route map: enumerate every HTTP endpoint registered. Verify none are hidden, undocumented, or serve as covert data access points.
- Webhook handlers: verify all incoming webhooks are authenticated and data is not forwarded to undocumented destinations.
- SSE streams: verify server-sent events contain only documented event types.
- CORS, authentication, rate limiting: verify these are correctly applied and cannot be bypassed.
- Embedded web assets (`rust-embed`): verify the `web/` directory contains only legitimate frontend files with no hidden scripts.

**Files:** `src/gateway/`, `web/` directory.

### AU-04: Agent Tools — Network (`src/tools/`, network-related subset)

**Objective:** Verify that network-capable tools (HTTP fetch, browser, web search) do not make unauthorized requests or leak data.

**What to look for:**
- `http_fetch` / `web_fetch` tools: verify they make requests only to user/agent-specified URLs, not to hardcoded destinations.
- Browser automation tools: verify they do not navigate to hidden URLs or exfiltrate page content.
- Web search tools: verify search queries are sent only to documented search APIs.
- Any tool that constructs outbound network requests: verify the destination and payload are fully derived from tool input parameters.

**Files:** Network-related files under `src/tools/` (http, web_fetch, browser, web_search, and similar).

### AU-05: Observability & Telemetry (`src/observability/`, ~2.4K lines)

**Objective:** Verify that metrics, tracing, and logging do not exfiltrate sensitive data.

**What to look for:**
- OpenTelemetry export: verify OTLP endpoints are user-configured only, with no hardcoded fallback destinations.
- Prometheus metrics: verify metric labels do not contain conversation content, API keys, or PII.
- Tracing spans: verify span attributes do not embed sensitive data.
- Log output: verify log messages at all levels do not print API keys, tokens, or conversation content (or if they do, only at debug/trace level with appropriate warnings).

**Files:** `src/observability/`, tracing configuration in `src/config/`.

### AU-06: Tunnel & WebSocket (`src/tunnel/`, ~1K lines)

**Objective:** Verify WebSocket tunneling is used only for documented channel connectivity and not as a covert data channel.

**What to look for:**
- All WebSocket connections: verify destinations are user-configured.
- Data transmitted over tunnels: verify it matches documented protocol framing.
- No hidden "heartbeat" or "keepalive" payloads that embed system info.

**Files:** `src/tunnel/`.

---

## Tier 2 — Credential & Data Handling

### AU-07: Authentication & Secrets (`src/auth/`, ~2.5K lines)

**Objective:** Verify that OAuth flows, API keys, and secret storage do not leak credentials.

**What to look for:**
- OAuth flows: verify redirect URIs, token exchange endpoints, and scopes match documented provider requirements. No extra scopes or hidden token forwarding.
- Secret store (ChaCha20-Poly1305 AEAD): verify encryption key derivation is sound, encrypted blobs are not transmitted anywhere, and decrypted secrets are not logged.
- Token persistence: verify tokens are stored only in expected local paths (`~/.zeroclaw/`) and not exfiltrated.
- API key handling throughout the codebase: grep for patterns where API keys are read and verify they flow only to their intended provider.

**Files:** `src/auth/`, secret storage in `src/config/` or `src/util.rs`.

### AU-08: Agent Tools — File I/O (`src/tools/`, file-related subset)

**Objective:** Verify file read/write tools respect sandbox boundaries and do not exfiltrate file contents.

**What to look for:**
- Path validation: verify `SecurityPolicy` sandbox checks cannot be bypassed (symlink escape, path traversal, TOCTOU race conditions).
- File content handling: verify read file contents flow only to the agent conversation, not to any network destination.
- File write: verify written content comes only from agent instructions, no hidden content injection.

**Files:** `src/tools/file_read.rs`, `src/tools/file_write.rs`, `src/tools/file_edit.rs`, related path validation in `src/security/`.

### AU-09: Memory & Persistence (`src/memory/`, ~8K lines)

**Objective:** Verify storage backends store data only locally and do not transmit it externally.

**What to look for:**
- SQLite: verify database files are created only in expected locations. No network-attached storage or remote sync.
- PostgreSQL (optional): verify connection strings are user-configured only.
- Qdrant (optional): verify vector DB endpoint is user-configured only.
- Stored data: verify conversation history, embeddings, and metadata are not copied to any external service beyond the configured backend.

**Files:** `src/memory/`.

---

## Tier 3 — Security Model & Control Flow

### AU-10: Security Policy (`src/security/`, ~7.3K lines)

**Objective:** Verify the security model (autonomy levels, path sandboxing, rate limiting) is correctly implemented and cannot be bypassed.

**What to look for:**
- Autonomy level enforcement: verify that ReadOnly/Supervised/Full levels are checked consistently before tool execution.
- Path sandboxing: verify `is_path_allowed()` and `is_resolved_path_allowed()` correctly prevent access outside the workspace, including symlink escape and `..` traversal.
- Rate limiting: verify limits are enforced and cannot be trivially bypassed.
- Approval workflow (`src/approval/`): verify approval state cannot be forged or skipped.
- Any code paths that bypass security checks (e.g., "admin mode", debug flags, environment variables that disable security).

**Files:** `src/security/`, `src/approval/`.

### AU-11: Agent Loop & Decision Logic (`src/agent/`, ~9K lines)

**Objective:** Verify the agent execution loop does not contain hidden behavior triggers.

**What to look for:**
- Agent loop: verify the assess-execute-verify cycle calls only user-visible tools and documented providers.
- Classifier/routing: verify message classification does not trigger hidden code paths based on specific input patterns (magic strings, steganographic triggers).
- System prompt construction: verify system prompts are built from documented sources (config, SOP) and do not inject hidden instructions.
- History management: verify conversation history is not transmitted to undocumented endpoints.

**Files:** `src/agent/`.

### AU-12: Configuration & Initialization (`src/config/`, `src/onboard/`, ~15K lines)

**Objective:** Verify configuration loading does not fetch remote configs or phone home during startup.

**What to look for:**
- Config loading: verify config is read only from local files (`~/.zeroclaw/`, project directory). No HTTP fetches during config parsing.
- Onboarding wizard: verify the setup flow does not transmit system information, hardware details, or user choices to any external service.
- Environment variable handling: verify no env vars enable hidden debug modes, disable security, or set exfiltration endpoints.
- Default config values: verify no defaults point to author-controlled infrastructure.

**Files:** `src/config/`, `src/onboard/`, `src/main.rs`.

---

## Tier 4 — Supply Chain & Remaining Modules

### AU-13: Dependency Audit (`Cargo.toml`)

**Objective:** Verify all dependencies are legitimate, widely-used crates with no known supply chain concerns.

**What to look for:**
- Each dependency in `Cargo.toml`: verify it is a well-known crate on crates.io with substantial download counts and clear provenance.
- Flag any dependencies that are obscure, have very few downloads, are maintained by unknown authors, or have suspiciously recent first-publish dates.
- Verify feature flags do not pull in unexpected transitive dependencies.
- Check for `build.rs` or proc-macro crates that execute code at compile time.
- Verify `wa-rs` family of crates (WhatsApp client) — these are niche and warrant closer inspection.

**Files:** `Cargo.toml`, `Cargo.lock` (if present).

### AU-14: Remaining Modules

**Objective:** Sweep remaining modules for any anomalies missed by targeted audits.

**What to look for:**
- `src/cron/` — scheduled tasks: verify no hidden scheduled jobs that activate after a delay.
- `src/daemon/` — service management: verify no unauthorized system-level changes.
- `src/hooks/` — event callbacks: verify hooks do not relay event data externally.
- `src/health/`, `src/heartbeat/` — verify health checks and heartbeats do not phone home.
- `src/identity.rs` — verify identity/workspace resolution does not transmit machine fingerprints.
- `src/skills/`, `src/skillforge/`, `src/sop/` — verify skill/SOP loading is local-only.
- `src/rag/` — verify RAG pipeline does not exfiltrate document content.
- `src/integrations/` — verify integration registry is local-only.
- `src/cost/` — verify cost tracking is local-only.
- `src/doctor/` — verify health checks don't phone home.
- `src/multimodal.rs`, `src/migration.rs`, `src/util.rs` — general sweep.

**Files:** All files listed above.

---

## Execution Plan

### Agent Assignment

| Unit | Priority | Est. Lines | Agents |
|------|----------|-----------|--------|
| AU-01: Providers | Tier 1 | 20K | 1 |
| AU-02: Channels | Tier 1 | 29K | 2-3 (split by channel group) |
| AU-03: Gateway | Tier 1 | 4.5K | 1 |
| AU-04: Tools (network) | Tier 1 | ~8K | 1 |
| AU-05: Observability | Tier 1 | 2.4K | 1 |
| AU-06: Tunnel | Tier 1 | 1K | 1 (combine with AU-03 or AU-05) |
| AU-07: Auth & Secrets | Tier 2 | 2.5K | 1 |
| AU-08: Tools (file I/O) | Tier 2 | ~5K | 1 |
| AU-09: Memory | Tier 2 | 8K | 1 |
| AU-10: Security Policy | Tier 3 | 7.3K | 1 |
| AU-11: Agent Loop | Tier 3 | 9K | 1 |
| AU-12: Config & Init | Tier 3 | 15K | 1 |
| AU-13: Dependencies | Tier 4 | N/A | 1 |
| AU-14: Remaining | Tier 4 | ~15K | 1 |

**Total: 14 audit units, ~12-15 agent invocations.**

### Parallelism

All audit units are independent and can run in parallel. Recommended batch structure:

- **Batch 1** (Tier 1): AU-01 through AU-06 — 6-8 agents in parallel
- **Batch 2** (Tier 2-3): AU-07 through AU-12 — 6 agents in parallel
- **Batch 3** (Tier 4): AU-13, AU-14 — 2 agents in parallel

Alternatively, run all 14 units simultaneously if agent capacity permits.

### Agent Prompt Template

Each agent receives:

```
You are conducting a security audit of the ZeroClaw Rust codebase.

AUDIT UNIT: [unit ID and title]
FILES TO REVIEW: [file paths]
CODEBASE ROOT: C:\UnitySrc\ZeroClaw\

OBJECTIVE: [objective from the unit description]

THREAT MODEL: You are looking for deliberate backdoors, data exfiltration,
covert channels, credential leakage, and security bypasses. This codebase
has provenance concerns (12-day-old repo, 160K lines, star farming indicators,
inconsistent author identity). Approach with appropriate suspicion.

WHAT TO LOOK FOR: [checklist from the unit description]

Read every file in scope. Do not skim. For each file, trace all outbound
network calls, all data flows involving sensitive content (API keys,
conversation text, file contents), and all security-relevant control flow.

REPORT FORMAT:
- Module: [files reviewed]
- Lines reviewed: [count]
- Findings: [each finding with severity, description, file:line evidence]
- Verdict: PASS / CONDITIONAL PASS / FAIL
```

### Completion Criteria

The audit is complete when:
1. All 14 units have produced reports.
2. No unit has a FAIL verdict (or FAIL findings have been reviewed and a mitigation plan exists).
3. A summary document consolidates all findings, listing every CRITICAL/HIGH finding with disposition.

### Output

Audit reports should be written to `docs/audit/` with one file per unit (e.g., `docs/audit/AU-01_providers.md`). A summary file `docs/audit/SUMMARY.md` consolidates the verdicts.
