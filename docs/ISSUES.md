# Backlot — Known Issues

Consolidated issue tracker across all crates.

---

## Flick

15 issues. All deferred/NIT severity.

### 1. `validate_resolved_from_provider_info` adapter could be inlined

**File:** `flick/src/validation.rs` · **Category:** Simplification

Thin wrapper that unpacks `ProviderInfo` fields and forwards to `validate_resolved`. Called from one site. The caller could call `validate_resolved` directly.

### 2. `validate_assistant_content` could fold into `validate_message_structure`

**File:** `flick/src/context.rs` · **Category:** Simplification

`validate_assistant_content` iterates all messages a second time to check one condition (empty assistant content). Could be merged into the existing `validate_message_structure` loop.

### 3. FlickResult construction duplicated in runner

**File:** `flick/src/runner.rs` · **Category:** Simplification

Two-step and single-step paths both construct `FlickResult` with `UsageSummary` in near-identical fashion.

### 4. `_ = compat` dead parameter in validate_resolved

**File:** `flick/src/validation.rs` · **Category:** Simplification

`validate_resolved` accepts `Option<&CompatFlags>` that is immediately discarded. Reserved for future use but adds noise to call sites.

### 5. `CompatFlags` placement in provider_registry

**File:** `flick/src/provider_registry.rs` · **Category:** Separation of concerns

`CompatFlags` describes provider behavioral quirks consumed by validation and providers, not registry-specific. Could move to a shared types module.

### 6. `flick_dir()` and `home_dir()` in provider_registry

**File:** `flick/src/provider_registry.rs` · **Category:** Separation of concerns

General path utilities unrelated to provider credential management. Other modules needing the flick directory must import from provider_registry.

### 7. `validate_resolved` naming

**File:** `flick/src/validation.rs` · **Category:** Naming

`validate_resolved` is vague. A name like `validate_config_against_provider` would communicate what is validated and against what.

### 8. `platform.rs` module name is broad

**File:** `flick/src/platform.rs` · **Category:** Naming

Currently contains only one Windows ACL function. `permissions.rs` or `fs_permissions.rs` would be more precise.

### 9. `crypto.rs` `provider` parameter name

**File:** `flick/src/crypto.rs` · **Category:** Naming

The `provider` parameter in `encrypt`/`decrypt` serves as AAD (additional authenticated data). The name is domain-specific rather than describing its cryptographic role.

### 10. `validation.rs` missing branch coverage

**File:** `flick/src/validation.rs` · **Category:** Testing

Missing tests for: ChatCompletions temperature > 2.0, reasoning+output_schema allowed on ChatCompletions, budget_tokens skipped on ChatCompletions, happy path.

### 11. `crypto.rs` missing invalid hex test

**File:** `flick/src/crypto.rs` · **Category:** Testing

`decrypt` has an error path for `hex::decode` failure but no test covers it.

### 12. `platform.rs` has zero test coverage

**File:** `flick/src/platform.rs` · **Category:** Testing

`restrict_windows_permissions` has no tests. A smoke test on Windows would catch regressions.

### 13. `CacheRetention::Long` TTL format may not match API

**File:** `flick/src/provider/messages.rs` · **Category:** Correctness

`CacheRetention::Long` emits `"ttl": "1h"` (string). Anthropic API documentation has shown both string and integer formats at different times. Verify against the current API whether `"1h"` or `3600` (integer seconds) is expected.

### 14. `CacheRetention` naming

**File:** `flick/src/config.rs` · **Category:** Naming

`CacheRetention` conflates "whether to cache" (the `None` variant disables injection entirely) with "how long to cache" (Short vs Long). A name like `CachePolicy` or `CacheMode` would cover both aspects more accurately.

### 15. Cache control test coverage gaps

**Files:** `flick/src/provider/chat_completions.rs`, `flick/src/config.rs`, `flick/src/runner.rs` · **Category:** Testing

Missing tests: (a) Chat Completions negative test asserting no `cache_control` in output, (b) `set_cache_retention` setter, (c) builder `cache_retention()` method, (d) `#[serde(skip)]` interaction with `deny_unknown_fields`, (e) `build_params` threading of cache_retention.

---

## Lot

69 NIT-level findings. 0 MUST FIX, 0 NON-CRITICAL. Generated from audit: 2026-03-24.

### Group 3 — Missing test coverage: lifecycle

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 16 | lot/src/unix.rs | 512-566 | `setup_stdio_fds` has no direct test coverage. The fd-aliasing logic (`effective_fd` helper, `redirected` tracking array) is only exercised indirectly via integration tests. The aliasing case (same fd for stdout and stderr) has zero coverage. Difficult to unit-test: runs in a forked child, requires real fd manipulation. |
| 17 | lot/tests/integration.rs | 1488-1636 | Tokio timeout tests verify timeout fires and fast-child completes, but don't verify child process cleanup after timeout. |
| 18 | lot/tests/integration.rs | 435-499 | `test_cleanup_after_drop` uses `echo` (short-lived), so assertions likely pass because `echo` already exited, not because drop killed it. A long-running child would actually test drop-triggered kill. |

### Group 4 — Silent failures in kill/signal/cleanup paths

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 19 | lot/src/linux/mod.rs | 569 | `kill_by_pid` ignores `libc::kill` return. Cannot distinguish success from permission denied. |
| 20 | lot/src/macos/mod.rs | 244-254 | `kill_by_pid` silently discards `libc::kill` return. Permission errors invisible. |

### Group 5 — TOCTOU in namespace mount point setup

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 21 | lot/src/linux/namespace.rs | 247-263 | TOCTOU window in `/tmp/lot-newroot-{pid}`. Operationally harmless: runs after `unshare(CLONE_NEWNS)`, mount operations are namespace-private. |

### Group 6 — Path canonicalization fallback

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 22 | lot/src/path_util.rs | 33-34 | `is_strict_parent_of` falls back to uncanonicalized path on `canonicalize_existing_prefix` failure. Harmless: callers in `policy.rs` have already canonicalized upstream. |

### Group 7 — Remaining correctness NIT

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 23 | lot/src/unix.rs | 519-526 | `effective_fd` returns first match in redirected array. Fragile if calling pattern changes, though safe with current 3-step logic. |

### Group 8 — Error handling in fork/child paths

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 24 | lot/src/linux/seccomp.rs | 447 | Test helper `fork_with_seccomp` doesn't check `waitpid` return value or child exit status. |
| 25 | lot/src/unix.rs | 377 | `child_bail` discards `libc::write` return. Defensible since `_exit(1)` follows. |

### Group 9 — Error handling in test helpers

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 26 | lot/src/unix.rs | 1148-1156 | Test helper `fork_pipe_writer` discards write return value. |
| 27 | lot/src/unix.rs | 1540-1549 | Test child branch discards `libc::write` return for stdout/stderr. |
| 28 | lot/src/linux/mod.rs | 792-794 | `waitpid` return value unchecked in 4 test functions. |
| 29 | lot/src/linux/namespace.rs | 399 | `create_mount_point_file` does not check `libc::close(fd)` return value. Production code. |

### Group 10 — Incorrect comments

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 30 | lot/src/macos/seatbelt.rs | 193 | Comment says "most-specific-match-wins" but SBPL uses last-match-wins. |
| 31 | lot/src/command.rs | 23 | Field comment says "Platform essentials are always included." Misleading for Windows (empty env -> null pointer -> child inherits parent's full environment). |

### Group 11 — Documentation and design doc mismatches

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 32 | docs/LOT_DESIGN.md | 250-262 | Graceful Degradation table missing `Unsupported` error variant. |
| 33 | lot/src/policy_builder.rs | 13-19, 83-84 | `read_path()` doc says "same-or-lower privilege sets" (plural), but read is the lowest. |

### Group 12 — Separation of concerns

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 34 | lot/src/linux/namespace.rs | 1-983 | 983-line file handles 4 concerns but only mount namespace setup is large; others are trivial. |
| 35 | lot/src/unix.rs | 259-485 | `read_two_fds` conflates poll event loop with data accumulation. `check_child_error_pipe` merges pipe reading, protocol decoding, and child reap/cleanup. |
| 36 | lot/src/linux/mod.rs | 581-608 | `test_helpers` module has generic fd utilities that aren't Linux-specific. |
| 37 | lot/src/linux/namespace.rs | 91-174 | `mount_system_paths` mixes path classification, mount execution, symlink creation, and network-policy-aware `/etc` mounting. |
| 38 | lot/src/macos/mod.rs | 46-215 | `spawn` is 170-line monolith. |

### Group 13 — Broad architectural simplification

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 39 | policy_builder.rs, policy.rs, lib.rs | — | Double validation: `build()` calls `validate()`, then `spawn()` calls `validate()` again. Intentional — `spawn()` validates because callers may construct policies via `SandboxPolicy::new()` directly, bypassing the builder. |

### Group 14 — Inconsistent errno capture in child_bail! macro

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 40 | lot/src/linux/mod.rs | 454 | `*libc::__errno_location()` passed directly to `child_bail!`. Inconsistent with other call sites that save errno to a local first. |
| 41 | lot/src/macos/mod.rs | 120, 161, 178 | Same inconsistency with `*libc::__error()`. Three call sites. |

### Group 15 — Naming: functions that don't match behavior

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 42 | lot/src/unix.rs | 252, 696-703, 621-630 | `close_pipe_fds` is generic not pipe-specific. `send_sigkill` name suggests fire-and-forget. `validate_kill_pid` returns `Option` not `Result`. |
| 43 | lot/src/linux/mod.rs | 104, 546, 581-608 | `close_inherited_fds` closes ALL fds not just inherited. `kill_and_cleanup` closes fds before killing. `write_fd` discards errors. |
| 44 | lot/src/linux/namespace.rs | 91-95, 298-299, 490-520 | `mount_system_paths` also creates symlinks. `execute_pivot_root` does pivot+chdir+umount+rmdir. `parse_submounts` includes prefix mount. |

### Group 16 — Duplicated platform code patterns

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 45 | lot/src/linux/namespace.rs | 193-216 | `mount_policy_paths` has three identical loops differing only in iterator and bind function. |
| 46 | lot/src/unix.rs | 34-68 | `.map_err(...)` repeated 5 times for `CString::new` in `prepare_prefork`. |
| 47 | lot/src/macos/seatbelt.rs | 109-123 | Three separate loops for read/write/exec paths emitting identical `file-read-metadata` rules. |

### Group 17 — Policy and builder duplication

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 48 | lot/src/policy.rs | 240-258 | `all_paths` and `grant_paths` have nearly identical bodies. |
| 49 | lot/src/policy.rs | 173-211 | `validate_deny_paths` takes three separate grant-path slices, immediately chains them. |
| 50 | lot/src/policy_builder.rs | 90-102, 115-129, 142-152 | `read_path`, `write_path`, `exec_path` implement same pattern. |
| 51 | lot/src/policy_builder.rs | 288-346 | `platform_exec_paths` and `platform_lib_paths` allocate `Vec<PathBuf>` of static strings. Could return arrays or static slices. |
| 52 | lot/src/policy_builder.rs | 177-185 | `deny_paths` is a thin loop wrapper. No batch methods. |
| 53 | lot/src/policy.rs | 215-234 | `canonicalize_collect` and `collect_validation_error` catch-all `Err(e)` arm is dead code. |
| 54 | lot/src/policy.rs | 426-436 | `valid_policy` helper used only once. ~20 tests share same boilerplate. |
| 55 | lot/src/policy.rs | 447-472, 1004-1020 | `empty_policy_rejected` and `empty_policy_error_mentions_at_least_one_path` test identical setup, different assertions. |

### Group 18 — Minor code-level cleanup

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 56 | lot/src/macos/seatbelt.rs | 230-261 | `collect_ancestor_dirs` removal loop has no effect (redundant, not dead). |
| 57 | lot/src/unix.rs | 97-106 | `CString::new("/dev/null")` can never fail. Dead error path. |
| 58 | lot/src/unix.rs | 252-257 | `close_pipe_fds` duplicates iteration pattern already in `UnixSandboxedChild::close_fds`. |
| 59 | lot/src/unix.rs | 273-307 | `read_two_fds` rebuilds `pollfds` and `fd_buffer_id` arrays every iteration. |
| 60 | lot/src/linux/namespace.rs | 331-354 | `mount_tmpfs_with` allocates `CString` for literal `"tmpfs"` on every call. |
| 61 | lot/src/linux/namespace.rs | 293-300 | `pivot_root` and `mount_proc_in_new_root` are one-line wrappers. |
| 62 | lot/src/macos/mod.rs | 221-261 | `MacosSandboxedChild` single-field newtype. `kill_and_cleanup` body identical to `Drop::drop`. |
| 63 | lot/src/env_check.rs | 23-40 | `is_dir_accessible` accepts separate slices checked with identical logic. |
| 64 | lot/src/path_util.rs | 16-26 | `is_descendant_or_equal` uses two-phase canonicalize-then-fallback. |
| 65 | lot/src/unix.rs | 636-670 | `delegate_unix_child_methods!` macro generates 8 trivial delegation methods. A `Deref` impl would be more idiomatic. |

### Group 19 — Test boilerplate reduction

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 66 | lot/src/linux/mod.rs | 751-898 | Four `close_inherited_fds_*` tests share identical boilerplate (~120 lines). |
| 67 | lot/src/linux/seccomp.rs | 459-690 | 8 test child bodies share identical boilerplate. |
| 68 | lot/src/error.rs | 41-113 | Six separate single-assertion tests verify `thiserror`'s `#[error("...")]` expansion. |
| 69 | lot/src/path_util.rs | 192-394 | `normalize_lexical` and `strict_parent_*` tests repeat `#[cfg]` gating. |
| 70 | lot/src/env_check.rs | 445-474 | Tests use `std::slice::from_ref(&grant)` instead of simpler `&[grant]`. |

### Group 20 — Remaining NIT-level test coverage gaps

| # | File | Line(s) | Description |
|---|------|---------|-------------|
| 71 | lot/src/lib.rs | 235-244 | `cleanup_stale` on non-Windows is a no-op. No test. |
| 72 | lot/src/lib.rs | 569-592 | `kill_by_pid` tests only verify absence of panics. |
| 73 | lot/src/policy.rs | 109-145 | `check_cross_overlap` with `AllowChildUnderParent` tested only indirectly. |
| 74 | lot/src/policy.rs | 148-169 | No test for intra-overlap within `read_paths` or `write_paths`. |
| 75 | lot/src/policy_builder.rs | 257-260 | `sentinel_dir()` has no test coverage. |
| 76 | lot/src/env_check.rs | 53, 77 | `validate_env_accessibility` has hidden dependency on host environment. |
| 77 | lot/src/env_check.rs | 161-195 | No test for first-match semantics with duplicate keys. |

---

## Reel

Issues grouped by severity, then by co-fixability.

### Group 7: Error Handling [NON-CRITICAL]

**7.1** `unwrap_or_default` masks extraction errors in tests — reel/src/agent.rs lines 478, 516. If `extract_text`/`extract_tool_calls` returns `Err`, test proceeds with empty data and gives misleading assertion failures.

**7.2** Multibyte truncation test assertion is a no-op — reel/src/tools.rs line 1392. `let _ = formatted.as_bytes()` cannot detect invalid truncation. Rust `String` is always valid UTF-8 by construction.

### Group 8: Naming [NON-CRITICAL]

**8.1** `response_hash` is actually `context_hash` — reel/src/agent.rs line 79. Name suggests response content hash but source is conversation context identifier.

**8.2** `nu-cache` / `NU_CACHE_DIR` should be `reel-cache` / `REEL_CACHE_DIR` — reel/build.rs lines 278, 283-288. Directory contains NuShell and ripgrep binaries plus config — not nu-specific.

### Group 9: Simplification [NON-CRITICAL]

**9.1** build.rs version string duplicated 11 times — reel/build.rs lines 27-98. `NU_VERSION`/`RG_VERSION` constants exist but are only used in download URLs, not in `asset_name` strings.

### Group 10: Documentation Accuracy [NIT]

**10.1** REEL_DESIGN.md round count off-by-one — docs/REEL_DESIGN.md line 100. Says "rounds < 50" but loop uses `for _round in 1..=50`.

**10.3** Root CLAUDE.md inapplicable C++ rules — CLAUDE.md (root) line 50. C++ exception handling rules are inapplicable — reel is Rust-only.

### Group 11: Dangling References & Cruft [NIT]

**11.1** Dangling reference to WINDOWS_SANDBOX.md — reel/src/nu_session.rs line 2294. References `docs/WINDOWS_SANDBOX.md` which does not exist.

**11.2** Issue tracker references in comments — reel/src/agent.rs, reel/src/nu_session.rs, reel/src/tools.rs, reel-cli/src/main.rs. Historical cruft issue references (`#1`, `#60`, `#56`, etc.).

### Group 12: Tool Definition Separation [NIT]

**12.1** tools.rs bundles 5 concerns in ~640 lines — reel/src/tools.rs lines 14-644. Grants, schema, translation, formatting, and dispatch all in one file.

### Group 13: Testing Gaps [NIT]

**13.1** `TempDir::new()` used instead of `TempDir::new_in()` — reel/src/nu_session.rs lines 934-935, 1040-1075; reel/src/tools.rs line 655. Not actually broken.

**13.2** `with_injected` is test-only — no downstream mock injection — reel/src/agent.rs lines 168-182. Design choice, not a bug.

**13.3** `duplicate_custom_tool_names` test replicates production logic — reel/src/agent.rs lines 1453-1471.

**13.4** `resolve_rg_binary` hard compile-time panic — reel/src/nu_session.rs lines 1185-1196. Uses `env!("NU_CACHE_DIR")` — hard panic if absent.

### Group 14: Error Handling [NIT]

**14.1** `emit_error` swallows serialization failure — reel-cli/src/main.rs lines 340-342.

**14.2** CI cgroup detection is fragile — .github/workflows/ci.yml lines 63-64, 70, 74-75.

### Group 15: Naming [NIT]

**15.1** `extract_text` doesn't convey "last" — reel/src/agent.rs lines 420-430.

**15.2** Misleading names in nu_session.rs — `dominated` means "compatible". `spawn_nu_process` also does MCP handshake. `try_spawn`/`try_eval` panic instead of returning errors.

**15.3** `tool_nu` reads as a noun — reel/src/tools.rs line 604.

**15.4** `_windows_` infix on cross-platform no-ops — reel-cli/src/main.rs lines 277, 316.

### Group 16: Simplification [NIT]

**16.1** CI jobs duplicate boilerplate — .github/workflows/ci.yml lines 41-142.

**16.2** agent.rs test injection complexity — reel/src/agent.rs lines 86-136, 148-151, 234-341. `skip_nu_spawn` leaks test concern into production struct.

**16.3** nu_session.rs duplicate blocks — reel/src/nu_session.rs. Four identical child-kill blocks. MCP handshake reimplements `rpc_call` inline.

**16.4** tools.rs repeated patterns — reel/src/tools.rs lines 313-372, 397-469. Boolean extraction repeated 4x, JSON parse-or-return-raw repeated 5x.

**16.5** sandbox.rs unused re-exports — reel/src/sandbox.rs lines 9-19, 33, 46.

**16.6** `parse_config` YAML round-trip just to strip one key — reel-cli/src/main.rs lines 103-132.

### Group 17: Separation of Concerns [NIT]

**17.1** nu_session.rs mixes protocol, resolution, and session management — reel/src/nu_session.rs. ~1200 production lines + ~1800 test lines in one file.

### Group 18: write_paths Testing Gaps [NIT]

**18.1** No test for evaluate respawn with non-empty write_paths.

**18.2** No test for write_paths outside project root.

---

## Vault

55 issues. 1 MUST FIX, 23 NON-CRITICAL, 31 NIT.

### MUST FIX

#### M3a. `emit_error_produces_json` is a false-positive test
- **File:** vault-cli/src/main.rs lines 348-352
- Does not call `emit_error` at all. Constructs independent `serde_json::json!` value — always passes regardless of `emit_error`'s behavior.

### NON-CRITICAL

#### Group N1: Documentation accuracy (3 issues)

**N1a.** VAULT_DESIGN.md public API listing incomplete — docs/VAULT_DESIGN.md line 13. Omits domain and observability types.

**N1b.** README record output description misleading — README.md line 57. Says "Outputs modified documents as JSON" but actually outputs `Vec<DocumentRef>`.

**N1c.** README omits plain-text warnings on stderr — vault-cli/src/main.rs lines 138-142; README.md line 69.

#### Group N2: storage.rs silent error suppression (2 issues)

**N2a.** `list_all_raw` silently skips unparseable version numbers — vault/src/storage.rs lines 406-413.

**N2b.** `extract_scope_comment` silently discards I/O errors — vault/src/storage.rs line 429.

#### Group N3: Error enum and type duplication (2 issues)

**N3a.** Four near-identical error enums — vault/src/lib.rs lines 188-298. BootstrapError, RecordError, QueryError, ReorganizeError all carry Io + LibrarianFailed variants.

**N3b.** Duplicate type wrappers in CLI — vault-cli/src/main.rs lines 60-73, 85-91, 118-127.

#### Group N4: Test coverage — operation orchestration (3 issues)

**N4a.** Vault facade methods have zero test coverage — vault/src/lib.rs lines 353-419.

**N4b.** CLI run_* functions have zero test coverage — vault-cli/src/main.rs lines 203-282.

**N4c.** reorganize.rs error paths and edge cases undertested — vault/src/reorganize.rs.

#### Group N5: Test coverage — assertion quality and determinism (5 issues)

**N5a.** `utc_now_iso8601` non-deterministic across all call sites.

**N5b.** `changelog_deserialize` test never asserts field values — vault/src/storage.rs lines 607-616.

**N5c.** `validate_derived` test is Unix-only — vault/src/storage.rs lines 940-982.

**N5d.** prompts.rs tests miss negative assertions — vault/src/prompts.rs lines 392-462.

**N5e.** `From<StorageError>` impls untested — vault/src/lib.rs lines 206-210, 239-248, 294-298.

#### Group N6: CI robustness (2 issues)

**N6a.** Windows Defender exclusion step lacks `continue-on-error` — .github/workflows/ci.yml lines 88-90.

**N6b.** CI test jobs lack timeout on macOS and Windows — .github/workflows/ci.yml lines 60, 74.

#### Group N7: Naming consistency (2 issues)

**N7a.** "invoker" parameter name should be `producer`/`responder`/`librarian`.

**N7b.** CHANGELOG.md contains JSONL, not Markdown — vault/src/storage.rs lines 146-148.

#### Group N8: Workspace dependency duplication (1 issue)

**N8a.** `reel` git dependency duplicated across workspace — vault/Cargo.toml, vault-cli/Cargo.toml.

#### Group N9: Observability test gaps (6 issues)

**N9a.** `SessionMetadata::from_run_result` untested — vault/src/lib.rs lines 93-139.

**~~N9b.~~** (RESOLVED) `api_latency_ms` testing covered.

**N9c.** Metadata propagation through operations untested — all mock librarians return `SessionMetadata::empty()`.

**~~N9d.~~** (RESOLVED) `build_usage_json` verbose test assertion added.

**~~N9e.~~** (RESOLVED) `build_usage_json` non-verbose path assertion added.

**N9f.** Session metadata types should be in a dedicated module — vault/src/lib.rs lines 24-162.

### NIT

#### Group T1: Separation of concerns — architectural placement (4 issues)

**T1a.** Utility functions misplaced in storage.rs — vault/src/storage.rs lines 448-520.

**T1b.** Validation logic mixed into storage — vault/src/storage.rs lines 296-349.

**T1c.** Query-specific parsing in librarian.rs — vault/src/librarian.rs lines 122-205.

**T1d.** Operation types defined in facade — vault/src/lib.rs lines 188-306.

#### Group T2: Test mock quality (3 issues)

**T2a.** No single mock combines argument capture and configurable success/failure.

**T2b.** Six mock structs where two or three would suffice.

**T2c.** Mock struct names don't match traits.

#### Group T3: Operation module error path testing (4 issues)

**T3a.** Bootstrap error paths untested — vault/src/bootstrap.rs.

**T3b.** Record error paths undertested — vault/src/record.rs.

**T3c.** Query error and prompt paths undertested — vault/src/query.rs.

**T3d.** `snapshot_derived` has no direct unit test — vault/src/storage.rs lines 355-370.

#### Group T4: storage.rs simplification (4 issues)

**T4a.** Redundant length check in `is_valid_raw_name` — vault/src/storage.rs lines 114-116.

**T4b.** Hand-rolled UTC timestamp formatting (30+ lines) — vault/src/storage.rs lines 486-520.

**T4c.** Duplicated regex base pattern in three `LazyLock` statics — vault/src/storage.rs lines 99-110.

**T4d.** Duplicated directory-walking boilerplate — vault/src/storage.rs lines 278-293, 355-370.

#### Group T5: librarian.rs testing, error handling, simplification (4 issues)

**T5a.** `ReelLibrarian` and `build_request` untestable — vault/src/librarian.rs lines 47-71.

**T5b.** `parse_bare_json` test incomplete — vault/src/librarian.rs lines 217-224.

**T5c.** `parse_query_response` manually walks JSON Value — vault/src/librarian.rs lines 122-176.

**T5d.** `extract_json_block` silently falls through to passthrough — vault/src/librarian.rs lines 179-205.

#### Group T6: prompts.rs simplification and naming (2 issues)

**T6a.** `RECORD_BLOCK` is a template, not a constant — vault/src/prompts.rs lines 133-147.

**T6b.** Four identical prompt builder pairs — vault/src/prompts.rs lines 122-296.

#### Group T7: storage.rs version-writing correctness and naming (3 issues)

**T7a.** TOCTOU race in version assignment — vault/src/storage.rs lines 249-273.

**T7b.** Dead fallback in `versions.last().map_or` — vault/src/storage.rs lines 267-271.

**T7c.** `write_raw_versioned` boolean parameter should be enum — vault/src/storage.rs lines 249-273.

#### Group T8: lib.rs simplification (1 issue)

**T8a.** Repeated `ReelLibrarian` construction — vault/src/lib.rs lines 353-419.

#### Group T9: CI simplification (1 issue)

**T9a.** CI test jobs could use matrix strategy — .github/workflows/ci.yml lines 41-91.

#### Group M4: Stale code comment (1 issue)

**M4a.** Stale "spec" reference in comment — vault/src/reorganize.rs line 45.

#### Group T10: Standalone nits (4 issues)

**T10a.** Nushell shell override replaces user env config — vault_shell.nu line 8.

**T10b.** Timestamp fallback hides system clock errors — vault/src/storage.rs lines 489-491.

**T10c.** `compute_changed` doesn't convey created-document inclusion — vault/src/storage.rs lines 454-467.

**T10d.** Step-number comments narrate self-documenting code — vault/src/record.rs lines 28-48.

### Integration Testing Findings

#### IT1a. Bootstrap requires pre-existing storage_root (F-002)
- **File:** vault/src/bootstrap.rs
- Bootstrap fails if directory doesn't exist yet. Should create the directory.
- **Workaround:** `mkdir` the storage root before calling bootstrap.

#### ~~IT2~~ (RESOLVED)
SessionMetadata now captures RunResult fields.

---

## Epic

75 issues. All non-critical.

### 1. `ReelAgent::new()` error paths untested
src/agent/reel_adapter.rs — Neither `build_model_registry()` nor `ProviderRegistry::load_default()` error path is tested. **Testing.**

### 2. Missing wire-type edge-case tests
src/agent/wire.rs — `DetectedStepWire` default timeout, `SubtaskWire` invalid magnitude. Previously missing items now covered by test audit cleanup. **Testing.**

### 3. `lot` dependency uses local path override
Cargo.toml — `lot = { path = "../lot" }` is a local dev override. Must revert to pinned git rev before merge. **Correctness.**

### 4. Hardcoded tier array in `build_model_registry`
src/agent/reel_adapter.rs — Iterates `[Model::Haiku, Model::Sonnet, Model::Opus]`. If `Model` gains variants, silently incomplete. **Fragility.**

### 5. Redundant error wrapping on provider registry load
src/agent/reel_adapter.rs — `.map_err(|e| anyhow!(...))` adds no information. Use `anyhow::Context`. **Simplification.**

### 6. `run_request` untested; adapter lost testability seam
src/agent/reel_adapter.rs — No tests verify grant/model/schema pass-through. `ReelAgent` always constructs real `reel::Agent`. **Testing.**

### 7. `custom_tools: Vec::new()` allocated per agent call
src/agent/reel_adapter.rs — Every `run_request` allocates empty vec. **Simplification.**

### 8. `RunResult` metadata discarded by `ReelAgent` adapter
src/agent/reel_adapter.rs — `run_request` extracts only `.output`, discarding `usage`, `tool_calls`, `response_hash`. **Feature gap.**

### 9. Output schemas missing `additionalProperties: false`
src/agent/wire.rs — No schema generator sets this. LLM may produce extra fields. **Spec compliance.**

### 10. Default model names during init may not match non-Anthropic providers
src/main.rs — Defaults use Anthropic model names. Non-Anthropic providers fail with opaque error. **Edge case.**

### 11. Decompose/design phases get NU grant (arbitrary shell access)
src/agent/reel_adapter.rs — `readonly_grant()` includes `ToolGrant::NU`. These phases only need file-read tools. **Least privilege.**

### 12. Assess and checkpoint hardcoded to `Model::Haiku`
src/agent/reel_adapter.rs — No override mechanism. Haiku may lack sufficient reasoning capacity. **Design.**

### 13. `assess_recovery` uses `Model::Opus` with no tools
src/agent/reel_adapter.rs — Recovery assessor gets `ToolGrant::empty()`, cannot inspect codebase. **Design.**

### 14. Prompt injection via unsanitized `TaskContext` fields
src/agent/prompts.rs — All `TaskContext` fields interpolated into prompts without sanitization. Goals originate from prior LLM output. **Security.**

### 15. Dual rationale sections in recovery prompt
src/agent/prompts.rs — Two rationale sections appear without clear distinction when both populated. **Clarity.**

### 16. No case/whitespace normalization on wire type string fields
src/agent/wire.rs — All string matching is exact. LLMs may return variant casing. **Robustness.**

### 17. README describes lot as "via reel" but epic depends on lot directly
README.md — epic calls `lot::appcontainer_prerequisites_met` and `lot::grant_appcontainer_prerequisites` directly. **Documentation.**

### 18. TUI `VaultBootstrapCompleted` handler doesn't track cost
src/tui/mod.rs — Bootstrap cost omitted from TUI running cost total. **Correctness.**

### 19. `std::mem::forget(tmp)` leaks TempDir in test helper
src/knowledge.rs — `make_dummy_vault()` leaks directories on every test run. **Testing.**

### 20. No orchestrator tests for vault integration paths
src/orchestrator.rs — All 4 vault integration points have zero test coverage. Vault is always `None` in existing tests. **Testing.**

### 21. `ResearchTool::execute` untested
src/knowledge.rs — Three branches have no test coverage. **Testing.**

### 22. Vault cost folding in `run_request` untested
src/agent/reel_adapter.rs — Field-by-field arithmetic has no test verifying correctness. **Testing.**

### 23. `SessionMeta` field-by-field accumulation is fragile
src/agent/reel_adapter.rs — Manually adds 7 fields. New fields silently omitted. Should be `AddAssign` or `merge`. **Fragility.**

### 24. Vault construction duplicates registry building
src/main.rs — Builds `ModelRegistry` and `ProviderRegistry` a second time. **Simplification.**

### 25. `SessionMeta::from_vault` placed far from type definition
src/knowledge.rs — Splits constructor API across two files. **Placement.**

### 26. `vault_content` variable name is directionally confusing
src/orchestrator.rs — Holds content destined *for* vault but reads as content *from* vault. **Naming.**

### 27. Module `knowledge.rs` name doesn't match contents
src/knowledge.rs — Contains vault-integration glue, not "knowledge". **Naming.**

### 28. `record_findings` called per-gap instead of batched
src/knowledge.rs — Each gap triggers a separate `vault.record()` call. Batching would reduce LLM costs. **Performance.**

### 29. `run_pipeline` has no test coverage (concrete dependencies)
src/knowledge.rs — Concrete `Arc<vault::Vault>` and `Arc<reel::Agent>` prevent unit testing. **Testing.**

### 30. Document name collision from 40-char truncation
src/knowledge.rs — Different questions with identical prefixes produce same document name. **Correctness.**

### 31. `ResearchScope::Project` name hides vault-inclusive behavior
src/knowledge.rs — `Project` scope means vault + codebase exploration. **Naming.**

### 32. Hand-coded JSON schemas rebuilt on every call
src/knowledge.rs — Could use `LazyLock` statics. Schema/struct drift risk. **Simplification.**

### 33. Wire types and schemas not in `agent/wire.rs`
src/knowledge.rs — Breaks project convention of placing wire types in `src/agent/wire.rs`. **Placement.**

### 34. `TempDir::new()` in knowledge tests uses system temp
src/knowledge.rs — Per CLAUDE.md, AppContainer sandboxing requires project-local dirs. **Testing.**

### 35. Stale test names reference old NU grant
src/agent/reel_adapter.rs — `execute_grant_includes_write_and_nu`, `readonly_grant_includes_nu_not_write`. **Cruft.**

### 36. Stale NU references in README/DESIGN
README.md and docs/EPIC_DESIGN.md — References old `NU` grant name. **Cruft.**

### 37. `file_level_review` in `ReelAgent` is verbatim copy of `verify`
src/agent/reel_adapter.rs — Identical except for prompt builder call. Extract shared helper. **Simplification.**

### 38. Duplicate failure-routing in `finalize_branch` for file-level review
src/orchestrator.rs — Duplicates the `VerificationOutcome::Fail` arm. **Simplification.**

### 39. `finalize_branch` reimplements verify+review inline
src/orchestrator.rs — Instead of calling `try_verify`. Two code paths must be kept in sync. **Separation.**

### 40. Missing graceful error degradation in `try_file_level_review`
src/orchestrator.rs — Agent errors propagate as fatal `OrchestratorError::Agent`. Should degrade like `try_verify`. **Correctness.**

### 41. `branch_skips_file_level_review` test relies on event assertion only
src/orchestrator.rs — Cannot distinguish "correctly skipped" from "called but passed". **Testing.**

### 42. Duplicate `record_to_vault` across orchestrator and task
src/orchestrator/mod.rs and src/task/leaf.rs — Same vault recording logic in both. **Separation.**

### 43. Duplicate `try_verify`/`try_file_level_review` across orchestrator and task
src/orchestrator/mod.rs and src/task/leaf.rs — Core agent interaction identical. **Separation.**

### 44. Duplicate `check_scope` across leaf and branch task modules
src/task/leaf.rs and src/task/branch.rs — Same logic in two Task methods. **Separation.**

### 45. `__agent_error__` sentinel string for error propagation
src/task/leaf.rs and src/orchestrator/mod.rs — Stringly-typed error channel. **Design.**

### 46. (Resolved) `ChildResponse` now used; `BranchResult` removed

### 47. `emit_usage_event` sends `phase_cost_usd: 0.0`
src/task/leaf.rs — Task-level sends 0.0 while orchestrator sends actual value. **Correctness.**

### 48. EPIC_DESIGN.md describes unimplemented features as current
docs/EPIC_DESIGN.md — Simplification review, aggregate simplification, user-level config described as current but not implemented. **Documentation.**

### 49. Testing gaps from orchestrator refactor
src/task/leaf.rs ~450 lines with zero unit tests. Mutation methods lack unit tests. **Testing.**

### 50. `ancestor_goals` may duplicate parent goal
src/orchestrator/context.rs — Parent goal appears in both `parent_goal` and `ancestor_goals`. **Correctness.**

### 51. Test uses `std::env::temp_dir()` for checkpoint test
src/orchestrator/mod.rs — Per CLAUDE.md, should use project-local dirs. **Testing.**

### 52. `BranchVerifyOutcome` duplicates `VerifyOutcome`
src/task/branch.rs — Structurally identical types. **Simplification.**

### 53. Duplicated supersede_pending loop in orchestrator
src/orchestrator/mod.rs — Same ~20-line pattern in `execute_branch` and `attempt_recovery`. **Separation.**

### 54. Recovery eligibility policy split across Task and Orchestrator
src/task/branch.rs and src/orchestrator/mod.rs — Policy duplicated across layers. **Separation.**

### 55. Event emission in `assess_and_design_recovery` violates design principle
src/task/branch.rs — Emits events and records to vault, contradicting "Task methods contain decision logic" principle. **Separation.**

### 56. No direct unit tests for branch Task methods
src/task/branch.rs — 7 methods tested only indirectly through orchestrator tests. **Testing.**

### 57. `handle_checkpoint` chains classification + adjust + full escalation pipeline
src/task/branch.rs — Could extract `escalate_to_recovery` for independent testing. **Separation.**

### 58. Parameterized test pairs could be further consolidated
src/agent/wire.rs and src/task/scope.rs — Table-driven tests possible. **Simplification.**

### 59. Parameterized test names use generic `_cases` suffix
src/task/branch.rs and src/task/leaf.rs — More descriptive names would improve readability. **Naming.**

### 60. MockBuilder locks mutexes during exclusive `&mut self` access
src/test_support.rs — Zero contention during build. Could hold plain fields and wrap only in `build()`. **Simplification.**

### 61. `MockBuilder::build()` takes `&mut self` instead of `self`
src/test_support.rs — Consuming `build(self)` would prevent accidental double-build. **Simplification.**

### 62. `decompose_one/two/three` are near-identical copy-paste
src/test_support.rs — A single `decompose_n(count)` would replace all three. **Simplification.**

### 63. Duplicate struct construction in MockBuilder leaf/verify families
src/test_support.rs — Shared helpers parameterized by queue reference would reduce duplication. **Simplification.**

### 64. Orchestrator resume tests share ~25-30 lines of state setup
src/orchestrator/tests.rs — A `make_resume_state` helper would consolidate boilerplate. **Simplification.**

### 65. Event-drain-and-assert pattern repeated in orchestrator tests
src/orchestrator/tests.rs — Extract `drain_events` or `assert_event_found` helper. **Simplification.**

### 66. `fix_subtasks_no_recursive_fix` bundles two independent test scenarios
src/orchestrator/tests.rs — If Part 1 fails, Part 2 never runs. **Separation.**

### 67. MockAgentService doesn't assert queues are drained after test
src/test_support.rs — Leftover mock responses silently ignored. **Testing.**

### 68. Duplicated event-draining pattern in leaf tests
src/task/leaf.rs — Same pattern as issue 65. **Simplification.**

### 69. `empty_tree()` helper should be `TreeContext::default()`
src/task/leaf.rs — Adding `#[derive(Default)]` would eliminate 12-line helper. **Simplification.**

### 70. `Services` construction duplicated across task test modules
src/task/leaf.rs and src/task/scope.rs — Should consolidate in `test_support.rs`. **Separation.**

### 71. Missing leaf-level test coverage for additional `execute_leaf` paths
src/task/leaf.rs — Four code paths lack direct tests. **Testing.**

### 72. Duplicated `build_tree_context` in EpicStore vs orchestrator/context.rs
epic/src/store.rs lines 172-284 vs epic/src/orchestrator/context.rs — ~110 lines of identical tree-context-building logic. EpicStore should delegate to the shared implementation like EpicState does. **Simplification.**

### 73. Duplicated `record_to_vault`, `emit_usage_event`, `emit_escalation` between node_impl.rs and leaf.rs
epic/src/task/node_impl.rs vs epic/src/task/leaf.rs — Near-identical implementations differing only in runtime dep source (`self.rt_arc()` vs `svc`). Tied to legacy orchestrator retention. **Simplification.**

### 74. Legacy orchestrator retained in parallel with cue::Orchestrator
epic/src/orchestrator/mod.rs — Full orchestration implementation (~900 lines) kept alongside cue::Orchestrator. Documented as next removal step in STATUS.md. **Simplification.**

### 75. AI-specific types in generic cue crate
cue/src/types.rs — `Model::Haiku/Sonnet/Opus`, `SessionMeta` (LLM tokens/cost), `AgentResult<T>`, `LeafResult`, `RecoveryPlan`, `TaskUsage`. cue/src/events.rs — `VaultBootstrapCompleted`, `VaultRecorded`, `VaultReorganizeCompleted`, `FileLevelReviewCompleted`, `UsageUpdated`. These embed AI/vault domain vocabulary into the generic orchestration framework. **Separation.**

### 76. No tests for cue::Orchestrator coordination logic
cue/src/orchestrator.rs — 722 lines, zero tests. Should have mock TaskNode/TaskStore tests. **Testing.**

### 77. No tests for EpicStore (TaskStore impl)
epic/src/store.rs — 285 lines, zero tests. DFS traversal, subtask creation, cross-task queries untested. **Testing.**

### 78. No tests for EpicTask (TaskNode impl)
epic/src/task/node_impl.rs — 796 lines, zero tests. Full leaf lifecycle, branch verification, recovery reimplemented and untested in this new form. **Testing.**

### 79. No tests for new Task decision methods
epic/src/task/mod.rs — `is_terminal()`, `resume_point()`, `forced_assessment()`, `needs_decomposition()`, `decompose_model()`, `registration_info()`, `can_attempt_recovery()` have zero unit tests. **Testing.**

### 80. No tests for new EpicState methods
epic/src/state.rs — `create_subtask`, `any_non_fix_child_succeeded`, `into_parts`/`from_parts` have zero tests. **Testing.**

### 81. EpicStore::as_state clones entire task map on every save
epic/src/store.rs lines 66-73 — Full deep-clone of all tasks on every checkpoint. Could serialize directly from internal HashMap. Also violates Rust naming convention: `as_` prefix implies cheap borrow, should be `to_state()`. **Performance/Naming.**

### 82. EpicStore::create_subtask silently defaults parent_depth to 0
epic/src/store.rs lines 138-142 — Uses `unwrap_or(0)` when parent not found instead of returning error. Masks store-corruption scenarios. **Correctness.**

### 83. branch_fix_loop passes without calling complete_task_verified
cue/src/orchestrator.rs line 453 — Task phase never transitions to Completed, no TaskCompleted event emitted. Pre-existing bug faithfully reproduced in extraction. **Correctness.**

### 84. VerifyOutcome vs VerificationOutcome confusion
cue/src/types.rs — Two near-identical enums for the same concept. `VerifyOutcome` only used by epic leaf retry logic, should stay in epic. **Naming.**

### 85. Stale module comments not mentioning legacy status
epic/src/orchestrator/mod.rs, epic/src/task/leaf.rs, epic/src/task/branch.rs — Comments describe full orchestrator/lifecycle responsibility without noting these are legacy paths superseded by cue+node_impl.rs. **Documentation.**

### 86. No unit tests for EventLog/EventSubscription
epic/src/events.rs — New infrastructure (EventLog, EventSubscription, EventEmitter<CueEvent> impl) has no direct unit tests. Shutdown semantics, subscribe-before-emit edge case, try_recv, len/is_empty/snapshot are covered only indirectly via orchestrator integration tests. **Testing.**

### 87. No tests for From<CueEvent> mapping
epic/src/events.rs — The 10-variant From<CueEvent> for Event mapping has no direct test coverage. A field-mapping typo would go undetected. **Testing.**

### 88. Orchestrator field named `transmitter` inconsistent with trait
cue/src/orchestrator.rs — The `transmitter` field holds an `EventEmitter<CueEvent>` and the private helper method is called `emit()`. The field name should be `emitter` to match the trait and method. **Naming.**

### 89. `traits` crate name is maximally generic
traits/ — The crate name `traits` collides conceptually with the Rust keyword and conveys no domain information. All other crates have distinctive names (cue, epic, flick, lot, reel, vault). Consider `backlot-traits` or similar. **Naming.**

### 90. Pre-existing cruft in epic/README.md
epic/README.md — Module structure lists legacy orchestrator entries (orchestrator/mod.rs as "Coordinator", services.rs as "Services<A>"). Missing store.rs and task/node_impl.rs entries. events.rs described as "channel types" (stale). **Documentation.**

### 91. Resolved issues in ISSUES.md not marked
docs/ISSUES.md — Issues 74, 85, and several others (42, 43, 45, 51, 53, 54) reference orchestrator/mod.rs logic that was removed with the legacy orchestrator. These are resolved or partially resolved but not annotated. **Documentation.**

### 92. Naming inconsistency: "verify" vs "review" in branch verification
epic/src/agent/mod.rs (lines 91-107) and epic/src/agent/prompts.rs (lines 325, 355, 384) — Trait methods use `verify_branch_*` but prompt builders use `build_branch_*_review`. Breaks convention established by `file_level_review`/`build_file_level_review` pair. **Naming.**

### 93. `build_verify` and `verify` names are now ambiguously scoped
epic/src/agent/prompts.rs (line 274), epic/src/agent/mod.rs (line 85) — These are now leaf-only (branch uses three-phase prompts) but names don't reflect the narrowed scope. **Naming.**

### 94. Missing error injection for branch verification in MockAgent
epic/src/test_support.rs — No mechanism to inject `Err` for `verify_branch_{correctness,completeness,simplification}`, so the `Err` path through `verify_branch` is untested. **Testing.**

### 95. `branch_verify_all_three_phases_pass` test duplicates `single_leaf`
epic/src/orchestrator/tests.rs — Same mock setup and assertions as `single_leaf`. Adds no unique verification of three-phase behavior. **Testing.**

### 96. `gaps_filled` double-counts for ProjectAndWeb scope
src/knowledge.rs lines 574-607 — Both the codebase exploration loop and web search loop independently increment `gaps_filled` for the same gap. A gap filled by both sources counts twice. The counter is informational only (displayed as "Gaps filled: N" to the calling agent), no control flow depends on it. **Correctness.**

### 97. `fill_method` match in `identify_gaps` disconnected from `ResearchScope`
src/knowledge.rs lines 271-276 — String-matches on `scope_label()` return values instead of being a method on `ResearchScope`. Adding a new scope variant could silently fall to the `_ =>` default arm. Should be `ResearchScope::fill_description()` colocated with the enum. **Separation.**

### 98. `fill_method` match untested
src/knowledge.rs lines 271-276 — No test verifies that all `scope_label()` values are covered by the match (the `_ =>` fallback arm would silently produce generic text for any unhandled scope). **Testing.**

### 99. `run_pipeline` with Web/ProjectAndWeb scopes untested
src/knowledge.rs lines 540-633 — The core behavioral change (scope-conditional codebase exploration and web search loops) has no integration test. Consistent with pre-existing gap for Project scope (issue 29). **Testing.**

### 100. `try_leaf_simplification_review` duplicates `try_file_level_review` structure
src/task/node_impl.rs — Both methods are ~25 lines with identical structure (get rt, build context, call agent, accumulate usage, emit event, match outcome). Could be a shared helper parameterized by agent call and event variant. Same duplication exists in the post-verify review chain between `leaf_finalize` and `try_verify`. **Simplification.**

### 101. No test asserts on `LeafSimplificationReviewCompleted` event at orchestrator level
src/orchestrator/tests.rs — The event is emitted but no orchestrator-level test verifies it. Unit-level coverage exists in `leaf.rs`. **Testing.**

### 102. Simplification review in fix loop's `try_verify` path lacks dedicated test
src/task/node_impl.rs lines 686-692 — In `try_verify` (called during fix retry loop), after verification passes and file-level review passes, simplification review runs. No test exercises a scenario where simplification fails specifically in this code path. **Testing.**

### 103. STATUS.md Phase line does not mention simplification review
docs/STATUS.md line 94 — The one-line Phase summary for Epic still reads "file-level review" but does not mention the new leaf simplification review. The Implemented list is current. **Documentation.**

---

## Cue

No standalone issues. All cue-related findings tracked under Epic (issues 72-91) as they concern the extraction boundary.

---

## Mech

### 104. mech/Cargo.toml declares unused dependencies
mech/Cargo.toml — Deliverable 1 only needs `thiserror`, but the manifest already pulls in `cue`, `reel`, `cel-interpreter`, `serde`, `serde_yml`, `schemars`, `jsonschema`, and `tokio`. These should be added in the deliverables that first use them to keep compile times and the dependency surface minimal. **Simplification.**

### 105. `SchemaValidationFailure` Display embeds full raw LLM output
mech/src/error.rs line 24 — The `#[error(...)]` format includes `{raw_output}`, which for realistic LLM outputs produces unwieldy single-line error messages. Keep the field for programmatic access but drop it from the Display format. **Simplification.**

### 106. `MechError::Validation` variant name conflicts with `SchemaValidationFailure`
mech/src/error.rs lines 108-115 — `Validation` is a load-time aggregate but its name does not distinguish it from the runtime `SchemaValidationFailure` (§10.2). Rename to `LoadValidation` or `WorkflowValidation` to match the doc comment's stated responsibility. **Naming.**

### 107. Mech schema: `Def` suffix applied inconsistently
mech/src/schema/mod.rs — Suffix used on `FunctionDef`, `BlockDef`, `TransitionDef`, `ContextVarDef` but not on `PromptBlock`, `CallBlock`, `CallEntry`, `AgentConfig`, `CompactionConfig`, `ParallelStrategy`. Pick one convention — either drop `Def` everywhere or apply it everywhere. Current mix is misleading. **Naming.**

### 108. Mech schema: `WorkflowFile` / `WorkflowDefaults` names misleading
mech/src/schema/mod.rs lines 44–80 — `WorkflowFile` holds the whole mech document including `functions:`, not a file handle. `WorkflowDefaults` holds the entire `workflow:` section (named agents, schemas, context) — not just "defaults". Consider `MechDocument` / `WorkflowSection` (or similar) for clarity. **Naming.**

### 109. Mech schema: `AgentConfig.grant` is singular but typed as `Vec<String>`
mech/src/schema/mod.rs lines 273–275 — Field is named `grant` (singular) but holds a list of grant flags. Either pluralize to `grants` or match the underlying `ToolGrant` naming. Verify against MECH_SPEC §5.5.1 YAML keyword before renaming. **Naming.**

### 110. Mech schema: `CallEntry.func` inconsistent with `CompactionConfig::r#fn`
mech/src/schema/mod.rs line 220 — `CallEntry` uses `func` + `#[serde(rename = "fn")]` while `CompactionConfig` uses `r#fn` directly for the same YAML key. Pick one. **Naming.**

### 111. Mech schema: `InferLiteral` wrapper type could be collapsed
mech/src/schema/mod.rs lines 309–315 — Single-variant enum exists only to serialize the string `"infer"`. Can be folded into `SchemaRef::Infer` as a unit variant with `#[serde(rename = "infer")]`, removing a type and the awkward `SchemaRef::Infer(InferLiteral::Infer)` match pattern. **Simplification.**

### 112. Mech schema: `full_example.yaml` placement under src/
mech/src/schema/full_example.yaml — Pure test fixture, used only via `include_str!` from `#[cfg(test)]`. Conventional home is `mech/tests/fixtures/` or `mech/src/schema/testdata/` to make non-source nature explicit. **Placement.**

### 113. Mech schema: re-export surface flattens the `schema` module boundary
mech/src/lib.rs lines 17–21 — 16 schema types are re-exported at the crate root while `pub mod schema` is also exposed, giving two canonical paths for every type. Either re-export only entry points (`WorkflowFile`, `parse_workflow`) or make `schema` `pub(crate)`. **Placement.**

### 114. Mech schema: empty `call: []` deserializes as `Uniform(vec![])` not an error
mech/src/schema/mod.rs lines 207–216 — Untagged enum discrimination biases empty lists toward `CallSpec::Uniform`. Spec §4.4/§5.2 requires non-empty in practice. Deferred to Deliverable 5 load-time validation. **Correctness (deferred).**

### 115. Mech schema: `extends` permitted on named agents, not just inline
mech/src/schema/mod.rs lines 266–292 — Per §12.1, `extends` is allowed on inline agent configs only, not on `workflow.agents.<name>` entries. Current single `AgentConfig` type does not enforce this split. Deferred to Deliverable 5, or fix via separate `NamedAgentConfig` / `InlineAgentConfig` types if parse-time enforcement is desired. **Correctness (deferred).**

### 116. Mech schema: single 770-line mod.rs could split into submodules
mech/src/schema/mod.rs — Spec wording said "src/schema/mod.rs and submodules". Natural seams: `blocks.rs`, `agent.rs`, `schema_ref.rs`, `workflow.rs`. Cheaper to split now, before Deliverables 3–7 add validators that co-locate with each type cluster. **Separation.**

### 117. Mech cel: guard error policy not enforced (§10.2)
mech/src/cel.rs — `CelExpression::evaluate_guard` propagates evaluation errors to the caller. Spec §10.2 says guard runtime errors should be treated as `false` (non-fatal, with a warning). Either add the policy here or document clearly that the D11 transition executor must wrap `evaluate_guard` and apply the false-on-error rule. **Correctness (deferred).**

### 118. Mech cel: `CelEvaluation` variant reused for namespace binding failure
mech/src/cel.rs — `Namespaces::to_context` converts `serde_json::Value` → `cel_interpreter::Value` via `to_value`, which is effectively infallible for well-formed JSON. The fallible path reports `MechError::CelEvaluation` with a synthetic `source_text: "<namespace {name}>"`, which is a variant shape mismatch. Either use `.expect("JsonValue -> cel Value is infallible")` or introduce a dedicated `NamespaceBind` variant. **Naming / simplification.**

### 119. Mech cel: thin coverage for render branches and multibyte template literals
mech/src/cel.rs — `append_rendered` branches for `Null`, `UInt`, `Float`, `Map` are untested; no test exercises multi-level nested field access in a template (e.g. `{{block.foo.bar.baz}}`); no test covers multibyte literal text around `{{...}}` (e.g. `"héllo {{input.name}}"`). Add these when convenient. **Testing.**

### 120. Mech cel: `block`/`meta` namespace names diverge from spec §7 (`blocks` + `output`)
mech/src/cel.rs — Module doc flags the discrepancy and defers reconciliation to Deliverable 8. Track explicitly so D8 revisits the namespace layout and either updates §7 or renames the fields. **Naming (deferred).**

### 121. `SchemaRegistry::validate` does not use `self`
mech/src/schema/registry.rs — `validate(&self, ..)` dispatches through `ResolvedSchema::validator()` without touching registry state. Falsely implies the registry is required to validate inline/infer resolutions. Move to a method on `ResolvedSchema` or a free function. **Separation.**

### 122. `ResolvedSchema::Infer` forces fake `SchemaInvalid` error on validate
mech/src/schema/registry.rs — Mixing a non-validator sentinel (`Infer`) into the same enum as real compiled validators forces `validate()` to synthesize a misleading `SchemaInvalid` error (name `<infer>`) for the deferred case. Cleaner: split into `enum ResolvedSchema { Named{..}, Inline(..) }` plus `enum SchemaResolution { Ready(ResolvedSchema), Deferred }` at the `resolve` boundary, or add a dedicated `SchemaInferDeferred` error variant. **Separation / Naming.**

### 123. `SchemaInvalid` variant overloaded for inline compile failures and deferred-infer
mech/src/error.rs — `SchemaInvalid { name, .. }` is used for (a) a named shared schema that fails to compile, (b) an inline schema that fails to compile (sentinel `name: "<inline>"`), and (c) validating against a deferred `Infer` marker (sentinel `name: "<infer>"`). The variant's `name` field's stated responsibility is "registered schema name"; sentinels conflate three distinct conditions. Split into dedicated variants or rename `name` to `source`. **Naming.**

### 124. `SchemaRef::Ref("$ref:path")` external-file case rejected as "malformed"
mech/src/schema/registry.rs (`parse_named_ref`) — External file refs like `$ref:./foo.json` (reserved for Deliverable 7) currently produce `SchemaRefMalformed`, but they are not malformed — they are unsupported/deferred. Introduce `SchemaRefUnsupported` or `SchemaRefExternalDeferred` so the diagnostic matches the condition. Revisit in D7. **Naming.**

### 125. `registry.rs` placement under `mech/src/schema/` conflates two "schema" senses
mech/src/schema/registry.rs — The file's own module doc explicitly flags the collision: `crate::schema` is the parse-only YAML AST while `registry` implements a JSON Schema runtime concern. Relocating to a sibling top-level module (`mech/src/json_schema.rs` or similar) would match the architectural layering the doc comment describes. **Placement.**

### 126. Registry test coverage gaps
mech/src/schema/registry.rs tests — Missing: (a) a 3+ node cycle (a→b→c→a) to exercise `chain` accumulation beyond length 2; (b) a multi-hop non-cyclic alias chain (c→b→a) to exercise the `loop { continue }` path in `follow_top_level_ref` more than once; (c) a `$ref:./other.json`-shaped input to pin the current "external file refs are rejected" contract until D7; (d) a cycle test using the string form `"$ref:#a"` (current cycle tests only use the object form). **Testing.**

### 127. Dataflow cycle error message inverts edge direction; duplicate reports possible
mech/src/validate.rs `detect_dataflow_cycles` (~line 902) — The error message says "`{node}` -> `{next}` closes a cycle in `depends_on`", but `depends_on` points from dependent to prerequisite, so the data edge runs `next → node`. Also, a single cycle may be reported multiple times from different DFS start points. **Correctness (low).**

### 128. `validate_named_agents` duplicate-reports missing extends target
mech/src/validate.rs (~line 763) — For N agents in a chain whose terminal `extends` points at a missing name, the "extends target not a named agent" error can be pushed up to N times. Dedupe or check each agent's own `extends` once in the top loop. **Correctness (low).**

### 129. Heavy Prompt/Call arm duplication in validate_block and validate_cel_and_templates
mech/src/validate.rs lines ~412-559 and ~1038-1215 — The Prompt and Call arms duplicate ~60 lines each iterating `depends_on`, `set_context`, `set_workflow`, and transitions. Extract per-kind helpers or a shared `validate_common_block_fields`. Also bundle `check_cel_expr`'s 8 parameters into a `CelCtx<'_>` struct (removes the `clippy::too_many_arguments` allow). **Simplification.**

### 130. Misleading `validate_agent_ref_with_defaults` pair
mech/src/validate.rs (~line 828 / ~867) — `validate_agent_ref` takes `&WorkflowDefaults`; `validate_agent_ref_with_defaults` takes `Option<&WorkflowDefaults>`. The `_with_defaults` suffix implies the other is "without defaults" — opposite of reality. Rename or restructure. **Naming.**

### 131. Hand-rolled dominator algorithm over-engineered for reverse-reachability need
mech/src/validate.rs `compute_dominators` (~line 1414) — The sole use is "does target_block reach cur_block through control-flow or depends_on?" which is plain reverse-reachability, not dominance. A combined predecessor-graph BFS from `cur_block` would replace ~60 lines of worklist iteration. **Simplification.**

### 132. `normalized_grants` name misleads; only caller checks `"write"` membership
mech/src/validate.rs (~line 1335) — Name suggests normalization but the function also *expands* grants (adds `tools` under various conditions). The sole caller uses only `normalized.contains("write")` which is equivalent to `ac.grant.iter().any(|g| g == "write")`. Inline or rename to `effective_grants`. **Naming / simplification.**

### 133. `CollectedRefs.block_refs` has a pointless outer Option
mech/src/validate.rs (~line 1587) — Field typed `Vec<Option<(String, Option<String>)>>` but the producer at line 1654 only ever pushes `Some(...)`. The outer Option is dead structure misleading readers. Drop to `Vec<(String, Option<String>)>`. **Cruft / simplification.**

### 134. Unused `_fn_name` parameter in validate_cel_and_templates
mech/src/validate.rs (~line 1038) — The `_fn_name: &str` parameter is unused (underscore-prefixed). Remove it rather than keep as dead API surface. **Cruft.**

### 135. CEL reference-extraction helpers belong in `mech::cel`
mech/src/validate.rs lines ~1519-1700 — `CollectedRefs`, `collect_references`, `walk`, `walk_member_subexprs`, `flatten_member_chain`, and `extract_template_exprs` operate purely on `cel_parser::Expression` and `${...}` template strings with no validator state. They belong in `mech/src/cel.rs` (or `cel::refs`) where future linters or the runtime renderer could share them. **Placement.**

### 136. `resolve_schema_value` and `value_matches_json_type` belong in schema/registry.rs
mech/src/validate.rs (~lines 1322 and 1507) — Pure schema-resolution / JSON-Schema predicates are validator-agnostic and mirror functionality already in `mech/src/schema/registry.rs`. Move next to the rest of the JSON Schema machinery. **Placement.**

### 137. `check_*` vs `validate_*` naming inconsistency
mech/src/validate.rs — Most methods are `validate_*` but `check_cel_expr`, `check_template`, `check_call_fn` break the pattern for the same "emit errors into report" responsibility. Rename to `validate_*`. **Naming.**

### 138. validate.rs (2606 lines) warrants promotion to `validate/` directory
mech/src/validate.rs — Single flat file mixes public API types (`ModelChecker`, `Location`, `ValidationIssue`, `ValidationReport`), the `Validator` walker, graph algorithms, CEL ref extraction, schema helpers, and 40+ inline tests. Promote to `validate/mod.rs` + `validate/model.rs` + `validate/report.rs` + `validate/walker.rs` mirroring the `schema/` directory layout. **Placement.**

### 139. Double CEL parse per expression
mech/src/validate.rs (~line 1233) — `cel_parser::parse(expr_src)` is called after `CelExpression::compile(expr_src)`. `compile` presumably parses internally; exposing the AST on `CelExpression` would avoid parsing every workflow expression twice. **Simplification / performance.**

### 140. `collects_multiple_errors` test is weak
mech/src/validate.rs (~line 2525) — Asserts only `r.errors.len() >= 2`. A regression where the same error is reported twice would satisfy this. Use `assert_err_contains` for each of the two specific expected errors, and consider exercising aggregation across different functions + different check categories (structural + graph + type) in one pass. **Testing.**

### 141. Missing dedicated passing fixtures for most §10.1 checks
mech/src/validate.rs — D5 spec requires each check have "at least one failing fixture AND one passing fixture". Many checks have only the failing side and rely on the global worked-example test for positive coverage. Rows lacking a dedicated positive test: invalid block name, reserved name, schema empty-required, context var types, `set_context`/`set_workflow`, dataflow DAG, transition target, call target, `n_of_m` with valid `n`, terminal validation, agent extends/grant/model/`$ref:#name`, input schema match. **Testing.**

### 142. `ModelChecker::knows` and bare `Location` re-export
mech/src/validate.rs (~line 42) and mech/src/lib.rs (~line 29) — `knows(...)` reads awkwardly; prefer `is_known` / `contains`. The trait itself might be better named `ModelRegistry` / `ModelResolver`. Separately, `Location` is re-exported unqualified at the crate root where it could collide with future parser/CEL error locations; keep it module-qualified or rename. **Naming / placement.**

### 143. External `$ref:path` schema/agent file resolution deferred to D7
mech/src/validate.rs (~line 694 and ~line 862) — `$ref:#name` is checked here; `$ref:path` (external file) is silently accepted. Spec §10.1 requires file existence checks at load time. Tracked to the Deliverable 7 workflow loader. **Tracking.**
