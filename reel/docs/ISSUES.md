# Known Issues

Issues grouped by co-fixability (can be addressed together in one pass).
Groups ordered by severity descending (MUST FIX → NON-CRITICAL → NIT), then by impact within severity.

---

## Group 7: Error Handling [NON-CRITICAL]

### 7.1 unwrap_or_default masks extraction errors in tests
- **File:** reel/src/agent.rs, lines 478, 516
- If `extract_text`/`extract_tool_calls` returns `Err`, test proceeds with empty data and gives misleading assertion failures.

### 7.2 Multibyte truncation test assertion is a no-op
- **File:** reel/src/tools.rs, line 1392
- `let _ = formatted.as_bytes()` cannot detect invalid truncation. Rust `String` is always valid UTF-8 by construction.

---

## Group 8: Naming [NON-CRITICAL]

### 8.1 response_hash is actually context_hash
- **File:** reel/src/agent.rs, line 79
- Name suggests response content hash but source is conversation context identifier.

### 8.2 nu-cache / NU_CACHE_DIR should be reel-cache / REEL_CACHE_DIR
- **File:** reel/build.rs, lines 278, 283-288
- Directory contains NuShell and ripgrep binaries plus config — not nu-specific.

---


## Group 9: Simplification [NON-CRITICAL]

### 9.1 build.rs version string duplicated 11 times
- **File:** reel/build.rs, lines 27-98
- `NU_VERSION`/`RG_VERSION` constants exist (lines 18-19) but are only used in download URLs, not in `asset_name` strings. Version bump requires editing 11 asset_name literals manually (6 Nu + 5 rg).

---

## Group 10: Documentation Accuracy [NIT]

### 10.1 DESIGN.md round count off-by-one
- **File:** docs/DESIGN.md, line 100
- Says "rounds < 50" but loop uses `for _round in 1..=50` (50 inclusive, agent.rs line 293). Off by one round.

### 10.3 Root CLAUDE.md inapplicable C++ rules
- **File:** CLAUDE.md (root), line 50
- C++ exception handling rules are inapplicable — reel is Rust-only. Not in this project's control.

---

## Group 11: Dangling References & Cruft [NIT]

### 11.1 Dangling reference to WINDOWS_SANDBOX.md
- **File:** reel/src/nu_session.rs, line 2294
- Comment references `docs/WINDOWS_SANDBOX.md` which does not exist. Comment-only; no runtime or build impact.

### 11.2 Issue tracker references in comments
- **Files:** reel/src/agent.rs, reel/src/nu_session.rs, reel/src/tools.rs, reel-cli/src/main.rs
- Issue references (`#1`, `#60`, `#56`, etc.) appear in both test comments and production doc-comments (e.g., nu_session.rs lines 177, 187, 386). Some are historical cruft ("Bare words removed in issue #65") violating CLAUDE.md's "No historical references" rule. Others are present-tense traceability ("Known limitation (issue #60)") which serve a documentation purpose — these are borderline.

---

## Group 12: Tool Definition Separation [NIT]

### 12.1 tools.rs bundles 5 concerns in ~640 lines
- **File:** reel/src/tools.rs, lines 14-644
- Grants, schema, translation, formatting, and dispatch all in one file.

---

## Group 13: Testing Gaps [NIT]

### 13.1 TempDir::new() used instead of TempDir::new_in()
- **Files:** reel/src/nu_session.rs (lines 934-935, 1040-1075), reel/src/tools.rs (line 655)
- Line 935 is in `policy_test_fixture` (sandbox test fixture) but the outer `TempDir::new()` is only the parent — nested `new_in()` calls create the sandbox-accessible paths. Lines 1040-1075 and tools.rs:655 are non-sandbox tests. Not actually broken in any case.

### 13.2 with_injected is test-only — no downstream mock injection
- **File:** reel/src/agent.rs, lines 168-182
- `#[cfg(test)]` only. Design choice, not a bug.

### 13.3 duplicate_custom_tool_names test replicates production logic
- **File:** reel/src/agent.rs, lines 1453-1471
- Test rebuilds the HashMap expression rather than calling production code.

### 13.4 resolve_rg_binary hard compile-time panic
- **File:** reel/src/nu_session.rs, lines 1185-1196
- Uses `env!("NU_CACHE_DIR")` — hard panic if absent, while other tests gracefully skip.

---

## Group 14: Error Handling [NIT]

### 14.1 emit_error swallows serialization failure
- **File:** reel-cli/src/main.rs, lines 340-342
- No output if `serde_json::to_string` fails. Serialization of this trivial struct cannot realistically fail.

### 14.2 CI cgroup detection is fragile
- **File:** .github/workflows/ci.yml, lines 63-64, 70, 74-75
- Parses `/proc/self/cgroup` without validation. `|| true` masks real controller enablement failures.

---

## Group 15: Naming [NIT]

### 15.1 extract_text doesn't convey "last"
- **File:** reel/src/agent.rs, lines 420-430
- Returns the last text block (reverse iterator) but name doesn't indicate this.

### 15.2 Misleading names in nu_session.rs
- **File:** reel/src/nu_session.rs, lines 350-354, 770-772, 1629/1637
- `dominated` means "compatible". `spawn_nu_process` also does MCP handshake. `try_spawn`/`try_eval` panic instead of returning errors.

### 15.3 tool_nu reads as a noun
- **File:** reel/src/tools.rs, line 604
- Executes the NuShell tool end-to-end but name reads as a noun phrase.

### 15.4 _windows_ infix on cross-platform no-ops
- **File:** reel-cli/src/main.rs, lines 277, 316
- Functions compile as no-ops on non-Windows.

---

## Group 16: Simplification [NIT]

### 16.1 CI jobs duplicate boilerplate
- **File:** .github/workflows/ci.yml, lines 41-142
- Five jobs (clippy, test-linux, test-macos, test-windows, build) duplicate checkout/toolchain/cache config. `build` job is redundant — `clippy --all-targets` already compiles all targets.

### 16.2 agent.rs test injection complexity
- **File:** reel/src/agent.rs, lines 86-136, 148-151, 234-341
- `ClientFactory`/`ToolExecutor` are production abstractions with default implementations — not test-only. `skip_nu_spawn` leaks test concern into the production struct. Timeout-wrapping pattern repeated 3x (lines 243, 286, 325).

### 16.3 nu_session.rs duplicate blocks
- **File:** reel/src/nu_session.rs, lines 158, 444, 467, 509, 872-908
- Four identical child-kill blocks (lock guard, pattern match `Some`, call `kill()`, ignore result) across Drop, `kill()`, and error handling. MCP handshake (lines 872-908) reimplements the `rpc_call` send-serialize-read-parse pattern inline instead of reusing it.

### 16.4 tools.rs repeated patterns
- **File:** reel/src/tools.rs, lines 313-372, 397-469
- Boolean extraction (`.get().and_then(as_bool).unwrap_or(false)`) repeated 4x. JSON parse-or-return-raw block repeated in all 5 `format_*_result` functions.

### 16.5 sandbox.rs unused re-exports
- **File:** reel/src/sandbox.rs, lines 9-19, 33, 46
- `is_elevated`, `_for_policy` variants never consumed within reel.

### 16.6 parse_config YAML round-trip just to strip one key
- **File:** reel-cli/src/main.rs, lines 103-132
- Serialize-deserialize exists only to remove a single key.

---

## Group 17: Separation of Concerns [NIT]

### 17.1 nu_session.rs mixes protocol, resolution, and session management
- **File:** reel/src/nu_session.rs, lines 34-66/524-621, 637-764
- JSON-RPC protocol layer (wire types, parsing, `rpc_call`) and tool/binary resolution (`resolve_tool_dir`, `resolve_tool_binary`, `resolve_config_files`, sandbox policy construction) mixed into a 3000-line session file. The concerns are genuinely distinct but the file also contains ~1800 lines of tests, so the production code is ~1200 lines — within range for a cohesive module.
