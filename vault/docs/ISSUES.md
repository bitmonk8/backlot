# Known Issues

**Severity scale:** MUST-FIX (functional impact or spec contract violation, fix before ship) · NON-CRITICAL (noticeable spec/impl mismatch, functionally acceptable) · NIT (cosmetic divergence)

| Severity | Count |
|---|---|
| MUST FIX | 1 |
| NON-CRITICAL | 21 |
| NIT | 31 |
| **Total** | **53** |

---

## MUST FIX

### Group M3: False-positive test (1 issue)

#### M3a. emit_error_produces_json is a false-positive test
- **File(s):** vault-cli/src/main.rs
- **Line(s):** 296-300
- **Description:** Does not call `emit_error` at all. Constructs an independent `serde_json::json!` value and asserts against that — always passes regardless of `emit_error`'s behavior.

---

## NON-CRITICAL

### Group N1: Documentation accuracy (4 issues)

User-facing docs contain inaccurate descriptions. All fixable in one documentation pass.

#### N1a. DESIGN.md public API listing incomplete
- **File(s):** docs/DESIGN.md
- **Line(s):** 13
- **Description:** File-tree annotation says lib.rs contains "Public API: Vault, VaultEnvironment, error types" — a brief summary, not an exhaustive inventory. Actual public exports also include VaultModels, RecordMode, Coverage, QueryResult, Extract, ReorganizeReport, DerivedValidationWarning, DocumentRef. The annotation omits domain types that external consumers need.

#### N1b. README record output description misleading
- **File(s):** README.md
- **Line(s):** 57
- **Description:** Says record "Outputs modified documents as JSON" but actually outputs `Vec<DocumentRef>` (document references/metadata, not document content).

#### N1c. README omits plain-text warnings on stderr
- **File(s):** vault-cli/src/main.rs, README.md
- **Line(s):** main.rs:138-142; README.md:69
- **Description:** README says "Errors are emitted as JSON to stderr" which is accurate for errors. However, `emit_warnings` also writes plain-text validation warnings to stderr on successful operations. The README is silent about this second stderr channel, which could mislead consumers who parse stderr expecting only JSON.

#### N1d. STATUS.md test count stale
- **File(s):** docs/STATUS.md
- **Line(s):** 75
- **Description:** Claims "105 tests" but actual count is 116.

### Group N2: storage.rs silent error suppression (2 issues)

Both are in storage.rs and silently discard errors that callers should be aware of.

#### N2a. list_all_raw silently skips unparseable version numbers
- **File(s):** vault/src/storage.rs
- **Line(s):** 406-413
- **Description:** `list_all_raw` silently skips files where the version number fails to parse as `u32` (e.g., overflow). No error or warning is emitted.

#### N2b. extract_scope_comment silently discards I/O errors
- **File(s):** vault/src/storage.rs
- **Line(s):** 429
- **Description:** `extract_scope_comment` uses `.ok()?` to silently discard I/O errors when reading derived files. Caller cannot distinguish "no scope" from "file unreadable." Contrast with `validate_derived` which surfaces read failures as warnings.

### Group N3: Error enum and type duplication (2 issues)

Both address unnecessary type duplication between vault and vault-cli that can be resolved together.

#### N3a. Four near-identical error enums
- **File(s):** vault/src/lib.rs
- **Line(s):** 48-158
- **Description:** BootstrapError, RecordError, QueryError, and ReorganizeError all carry Io + LibrarianFailed variants. QueryError and ReorganizeError are structurally identical. Consolidation would eliminate six duplicate variant definitions and two identical `From<StorageError>` impls (BootstrapError and ReorganizeError both stringify all StorageError variants into Io).

#### N3b. Duplicate type wrappers in CLI
- **File(s):** vault-cli/src/main.rs
- **Line(s):** 60-73, 85-91, 118-127
- **Description:** `ConfigModels` duplicates `VaultModels` and `RecordModeArg` duplicates `RecordMode`. Adding `Deserialize`/`ValueEnum` derives upstream would eliminate both (~30 lines).

### Group N4: Test coverage — operation orchestration (3 issues)

Core operation functions and their wiring have zero test coverage.

#### N4a. Vault facade methods have zero test coverage
- **File(s):** vault/src/lib.rs
- **Line(s):** 213-273
- **Description:** The four public async `Vault` methods have zero test coverage. `Vault` holds concrete types with no trait abstraction, making unit testing impossible without a live `reel::Agent`. A regression in ReelLibrarian wiring would not be caught.

#### N4b. CLI run_* functions have zero test coverage
- **File(s):** vault-cli/src/main.rs
- **Line(s):** 97-103, 173-230
- **Description:** Zero test coverage for `run_bootstrap`, `run_query`, `run_record`, `run_reorganize`. Hard-coded dependencies on stdin/stderr/reel globals make them untestable. No integration test harness exists.

#### N4c. reorganize.rs error paths and edge cases undertested
- **File(s):** vault/src/reorganize.rs
- **Line(s):** 23-71, 152-201
- **Description:** No tests cover error paths for `snapshot_derived()` or `build_reorganize_prompt()` failures. No test for reorganize on an empty vault ("all new → all restructured"). Tests use `any()` without asserting total count — overcounting not caught.

### Group N5: Test coverage — assertion quality and determinism (5 issues)

Existing tests that pass but don't meaningfully validate behavior.

#### N5a. utc_now_iso8601 non-deterministic across all call sites
- **File(s):** vault/src/storage.rs, vault/src/bootstrap.rs, vault/src/record.rs, vault/src/reorganize.rs
- **Line(s):** storage.rs:487-503; bootstrap.rs:48; record.rs:52; reorganize.rs:58
- **Description:** `utc_now_iso8601()` calls `SystemTime::now()` directly with no injection point. Tests can only validate structural format, not actual values. No test at any call site verifies the timestamp value written to changelog entries.

#### N5b. changelog_deserialize test never asserts field values
- **File(s):** vault/src/storage.rs
- **Line(s):** 607-616
- **Description:** `changelog_deserialize_from_spec_examples` deserializes entries but never asserts field values. Wrong field mapping would still pass.

#### N5c. validate_derived test is Unix-only
- **File(s):** vault/src/storage.rs
- **Line(s):** 940-982
- **Description:** `validate_derived_unreadable_file_warns_and_continues` is `#[cfg(unix)]` only. No equivalent test for Windows — the error path has zero coverage on the actual development OS.

#### N5d. prompts.rs tests miss negative assertions
- **File(s):** vault/src/prompts.rs
- **Line(s):** 392-462
- **Description:** Record prompt tests don't assert `RAW_FILENAME_PLACEHOLDER` is absent from final output. Query prompt test doesn't assert absence of write-permitting scope restriction (swapped scopes would pass).

#### N5e. From<StorageError> impls untested
- **File(s):** vault/src/lib.rs
- **Line(s):** 66-70, 99-108, 154-158
- **Description:** `From<StorageError>` impls for error types are untested. BootstrapError and ReorganizeError stringify all StorageError variants into Io, losing structure — no test confirms this is intentional.

### Group N6: CI robustness (2 issues)

Both are CI workflow hardening for the same file.

#### N6a. Windows Defender exclusion step lacks continue-on-error
- **File(s):** .github/workflows/ci.yml
- **Line(s):** 88-90
- **Description:** The Windows Defender exclusion step lacks `continue-on-error: true`. Runner privilege changes would break the Windows test job.

#### N6b. CI test jobs lack timeout on macOS and Windows
- **File(s):** .github/workflows/ci.yml
- **Line(s):** 60, 74
- **Description:** macOS and Windows test jobs lack `timeout-minutes`. A hung test burns 6 hours of CI credits before failing.

### Group N7: Naming consistency (2 issues)

Cross-cutting naming mismatches affecting API clarity.

#### N7a. "invoker" parameter name across all operations
- **File(s):** vault/src/bootstrap.rs, record.rs, query.rs, reorganize.rs, lib.rs, test_support.rs
- **Line(s):** bootstrap.rs:20; record.rs:19; query.rs:16; reorganize.rs:23; lib.rs:217
- **Description:** The module is named `librarian`, production struct is `ReelLibrarian`, test mocks use `*Librarian` suffixes, but all operation parameters name the dependency `invoker`. For query.rs, `invoker` has bound `QueryResponder` — a responder does not "invoke." Should be `producer`, `responder`, or `librarian`.

#### N7b. CHANGELOG.md contains JSONL, not Markdown
- **File(s):** vault/src/storage.rs
- **Line(s):** 146-148
- **Description:** The file on disk is named `CHANGELOG.md` but contains JSONL content. The extension is misleading.

### Group N8: Workspace dependency duplication (1 issue)

#### N8a. reel dependency duplicated across workspace
- **File(s):** vault/Cargo.toml, vault-cli/Cargo.toml
- **Line(s):** (both files)
- **Description:** `reel` git dependency with `rev = "a6be158"` specified independently in both crates. Should use `[workspace.dependencies]`.

---

## NIT

### Group T1: Separation of concerns — architectural placement (4 issues)

Entities located in the wrong module. Addressing these together produces a coherent refactor.

#### T1a. Utility functions misplaced in storage.rs
- **File(s):** vault/src/storage.rs
- **Line(s):** 448-520
- **Description:** `utc_now_iso8601()`, `days_to_civil()`, `compute_changed()`, and `compute_deleted()` are general-purpose utilities with no dependency on `Storage` or the filesystem. Belong in a utility module.

#### T1b. Validation logic mixed into storage
- **File(s):** vault/src/storage.rs
- **Line(s):** 296-349
- **Description:** `validate_derived` performs content-level validation (title, scope comment format) — a validation/linting concern distinct from file I/O.

#### T1c. Query-specific parsing in librarian.rs
- **File(s):** vault/src/librarian.rs
- **Line(s):** 122-205
- **Description:** `parse_query_response` and `extract_json_block` are query-specific deserialization, not librarian abstraction concerns. Creates three-way coupling: prompts defines format, librarian parses it, query orchestrates.

#### T1d. Operation types defined in facade
- **File(s):** vault/src/lib.rs
- **Line(s):** 48-170
- **Description:** Operation-specific error types, result types, and domain types are all defined in `lib.rs` rather than alongside their respective operations.

### Group T2: Test mock quality (3 issues)

All in test_support.rs. Fixing mock naming, consolidation, and design gaps together.

#### T2a. Test mock design gaps
- **File(s):** vault/src/test_support.rs
- **Line(s):** 19-63
- **Description:** No single mock combines both argument capture and configurable success/failure. Testing that a failing librarian was called with correct arguments is impossible with existing mocks.

#### T2b. Excessive mock struct count
- **File(s):** vault/src/test_support.rs
- **Line(s):** 19-219
- **Description:** Six mock structs where two or three would suffice. `NoOpLibrarian`, `BadNameLibrarian`, `DeletingLibrarian` could use `MockLibrarian::succeeding` with closures. Also, `Mutex::lock()` error handling is unnecessarily cautious for test code.

#### T2c. Test mock struct names don't match traits
- **File(s):** vault/src/test_support.rs
- **Line(s):** 17-219
- **Description:** All mock structs named `*Librarian` but implement `DerivedProducer`/`QueryResponder` traits. Names should reflect the trait being mocked.

### Group T3: Operation module error path testing (4 issues)

Error paths across bootstrap, record, query, and storage are undertested. Can be addressed in a single test-writing pass.

#### T3a. bootstrap error paths untested
- **File(s):** vault/src/bootstrap.rs
- **Line(s):** 29, 88-108
- **Description:** No test covers `create_directories()` failure error path; `BootstrapError::Io` variant never exercised. `bootstrap_passes_correct_prompt_and_query` uses `CapturingLibrarian` with `None` writer, relying on empty-directory validation succeeding — fragile if validation becomes stricter.

#### T3b. record.rs error paths undertested
- **File(s):** vault/src/record.rs
- **Line(s):** 32, 44, 47
- **Description:** No tests cover error paths for `snapshot_derived()`, `validate_derived()`, or `append_changelog()` failures. Five `?` propagation points; only two tested.

#### T3c. query.rs error and prompt paths undertested
- **File(s):** vault/src/query.rs
- **Line(s):** 21-22, 185-194
- **Description:** No test covers `build_query_prompt` failure error path. `query_passes_correct_prompt_and_message` uses fragile `contains` checks with no assertion that the prompt includes the vault document listing.

#### T3d. snapshot_derived has no direct unit test
- **File(s):** vault/src/storage.rs
- **Line(s):** 355-370
- **Description:** `snapshot_derived` has no direct unit test. Exercised only indirectly via integration tests in record.rs and reorganize.rs.

### Group T4: storage.rs simplification (4 issues)

Redundant or duplicated code within storage.rs, fixable in one pass.

#### T4a. Redundant length check in is_valid_raw_name
- **File(s):** vault/src/storage.rs
- **Line(s):** 114-116
- **Description:** `len() >= 2` is redundant — the regex already requires at least 2 characters.

#### T4b. Hand-rolled UTC timestamp formatting
- **File(s):** vault/src/storage.rs
- **Line(s):** 486-520
- **Description:** Hand-rolled calendar algorithm (30+ lines) for ISO 8601 formatting. Could be a one-liner with a datetime library.

#### T4c. Duplicated regex base pattern
- **File(s):** vault/src/storage.rs
- **Line(s):** 99-110
- **Description:** Three `LazyLock<Regex>` statics repeat the same `[A-Z][A-Z0-9_]*[A-Z0-9]` base pattern. Could derive from a single constant.

#### T4d. Duplicated directory-walking boilerplate
- **File(s):** vault/src/storage.rs
- **Line(s):** 278-293, 355-370
- **Description:** `list_derived` and `snapshot_derived` share identical directory-walking boilerplate. Could extract a shared helper.

### Group T5: librarian.rs testing, error handling, and simplification (4 issues)

Testing gaps and error handling in librarian.rs, addressable together.

#### T5a. ReelLibrarian and build_request untestable
- **File(s):** vault/src/librarian.rs
- **Line(s):** 47-71
- **Description:** `ReelLibrarian` and `build_request` have zero unit test coverage. Hard dependency on `reel::Agent` prevents mock injection.

#### T5b. parse_bare_json test incomplete
- **File(s):** vault/src/librarian.rs
- **Line(s):** 217-224
- **Description:** `parse_bare_json` test does not assert the `content` field of the extract. Broken content parsing would not be caught.

#### T5c. parse_query_response manually walks JSON Value
- **File(s):** vault/src/librarian.rs
- **Line(s):** 122-176
- **Description:** Manually walks `serde_json::Value` fields (~50 lines). `Deserialize` derives on `QueryResult`/`Extract`/`Coverage` would reduce to a one-liner.

#### T5d. extract_json_block silently falls through to passthrough
- **File(s):** vault/src/librarian.rs
- **Line(s):** 179-205
- **Description:** `extract_json_block` returns the entire input trimmed when no fenced block is found, rather than returning an error. The caller (`parse_query_response`) will then fail at JSON parse, producing a less informative error message.

### Group T6: prompts.rs simplification and naming (2 issues)

Both in prompts.rs, addressing template naming and builder duplication.

#### T6a. RECORD_BLOCK is a template, not a constant
- **File(s):** vault/src/prompts.rs
- **Line(s):** 133-147
- **Description:** Contains a placeholder requiring runtime `.replace()` but is named like the other verbatim `*_BLOCK` constants. `RECORD_BLOCK_TEMPLATE` would signal the post-processing requirement.

#### T6b. Four identical prompt builder pairs
- **File(s):** vault/src/prompts.rs
- **Line(s):** 122-296
- **Description:** Four builders follow identical two-layer pattern. A single parameterized builder would cut eight functions to one.

### Group T7: storage.rs version-writing correctness and naming (3 issues)

All concern write_raw_versioned in storage.rs.

#### T7a. TOCTOU race in version assignment
- **File(s):** vault/src/storage.rs
- **Line(s):** 249-273
- **Description:** Concurrent callers of `write_raw_versioned` can assign the same version number, causing silent overwrites. No file locking or `O_EXCL` is used to prevent the race.

#### T7b. Dead fallback in versions.last().map_or
- **File(s):** vault/src/storage.rs
- **Line(s):** 267-271
- **Description:** `versions.last().map_or(1, ...)` has a dead fallback — `versions` is guaranteed non-empty by the preceding `is_empty()` check. `.unwrap()` expresses intent directly.

#### T7c. write_raw_versioned boolean parameter
- **File(s):** vault/src/storage.rs
- **Line(s):** 249-273
- **Description:** Takes a `new_series` boolean. An enum (`VersionMode::NewSeries | VersionMode::Append`) or two methods would be more self-documenting.

### Group T8: lib.rs simplification (1 issue)

#### T8a. Repeated ReelLibrarian construction
- **File(s):** vault/src/lib.rs
- **Line(s):** 217-272
- **Description:** Every `Vault` method constructs `ReelLibrarian` identically. A private helper would reduce four repetitions.

### Group T9: CI simplification (1 issue)

#### T9a. CI test jobs could use matrix strategy
- **File(s):** .github/workflows/ci.yml
- **Line(s):** 41-91
- **Description:** Three near-identical test jobs could be a single matrix job, eliminating ~30 lines of duplication.

### Group M4: Stale code comment (1 issue)

#### M4a. Stale "spec" reference in comment
- **File(s):** vault/src/reorganize.rs
- **Line(s):** 45
- **Description:** Comment says "per spec's ReorganizeReport fields" — the word "spec's" references the deleted SPEC.md. The field descriptions in the comment (merged/restructured/deleted) are still accurate; only the attribution is stale. DESIGN.md now documents ReorganizeReport but not the individual field semantics.

### Group T10: Standalone nits (4 issues)

Unrelated low-impact issues not suitable for co-fixing.

#### T10a. Nushell shell override replaces user env config
- **File(s):** vault_shell.nu
- **Line(s):** 8
- **Description:** `--env-config` replaces the user's normal `env.nu`, potentially breaking their `config.nu` which may depend on it. No comment documents this tradeoff.

#### T10b. Timestamp fallback hides system clock errors
- **File(s):** vault/src/storage.rs
- **Line(s):** 489-491
- **Description:** `duration_since(UNIX_EPOCH)` error is silently replaced with `Duration::default()` via `unwrap_or_default()`. On misconfigured systems this produces "1970-01-01T00:00:00Z" with no indication of failure.

#### T10c. compute_changed doesn't convey created-document inclusion
- **File(s):** vault/src/storage.rs
- **Line(s):** 454-467
- **Description:** Returns both created and modified documents. Name doesn't convey that newly-created documents are included.

#### T10d. Step-number comments narrate self-documenting code
- **File(s):** vault/src/record.rs
- **Line(s):** 28-48
- **Description:** Six `// Step N:` comments narrate what self-documenting function calls already express.

---

## Integration Testing Findings

Issues discovered during rig end-to-end integration testing.

### Group IT1: Bug (1 issue)

#### IT1a. Bootstrap requires pre-existing storage_root (F-002)
- **File(s):** vault/src/bootstrap.rs
- **Description:** `vault bootstrap` fails with "storage root does not exist or is not a directory" if the directory doesn't exist yet. Bootstrap is the initialization command — it should create the directory.
- **Workaround:** `mkdir` the storage root before calling bootstrap.

### Group IT2: Observability (2 issues)

#### IT2a. No token usage in CLI output (F-003)
- **File(s):** vault-cli/src/main.rs
- **Description:** No vault command exposes token usage or cost information. Adding usage stats to each command's JSON output would enable budget tracking across operations.

#### IT2b. Discards usage/cost data from reel (F-005)
- **File(s):** vault/src/librarian.rs
- **Description:** Vault calls `agent.run()` but discards the returned `RunResult` fields `usage` and `tool_calls` (assigned to `_result` in librarian.rs). Combined with IT2a, there is no observability into token consumption for any vault operation.
- **Note:** Blocked downstream by reel issue 19.2 (cache token fields stripped) and flick issue 15 (no prompt caching). Full observability requires fixes across all three layers.
