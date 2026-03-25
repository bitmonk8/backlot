# Known Issues

Issues grouped by co-fixability within severity. Groups ordered by impact (descending).

---

## NON-CRITICAL

### NC0: Vault::bootstrap() facade issues (lib.rs)

Both are design issues in the thin `Vault::bootstrap()` wrapper.

- **Facade coupled to ReelLibrarian** (`lib.rs:109-113`) — `Vault::bootstrap()` hardcodes `ReelLibrarian`. The underlying `bootstrap::run()` already accepts generic `L: LibrarianInvoker` and is fully tested with mocks. Only the 5-line facade wrapper is untestable without real infrastructure.
- **Warnings printed to stderr and discarded** (`lib.rs:116-121`) — `bootstrap::run()` returns `Vec<DerivedValidationWarning>`, but `Vault::bootstrap()` prints them via `eprintln!` and returns `Ok(())`. Caller cannot inspect or act on warnings. `eprintln!` is presentation-layer behavior that does not belong in a library API.

### NC1: Agent result handling and librarian testability (librarian.rs)

Both in `librarian.rs`; fixing result handling is prerequisite for meaningful tests.

- **Agent run success value discarded** (`librarian.rs:65-71`) — `RunResult<String>` from `agent.run()` is bound to `_result` and unused. Errors propagate via `.map_err()`; only the success payload is discarded. Caller cannot inspect agent output on success.
- **Zero test coverage; config coupled to execution** (`librarian.rs:1-73`) — No unit tests. `produce_derived` mixes config assembly with agent execution, preventing unit testing of config logic. Grant construction, `write_paths` wiring, and `RequestConfig` assembly are unverified. Requires reel to expose test utilities via a `testing` feature or a public test constructor.

### NC2: SPEC-implementation divergence (storage.rs, prompts.rs, lib.rs)

All are cases where implementation does not match SPEC; fixing requires deciding whether to update code or SPEC.

- **DocumentRef struct diverges** (`storage.rs:64-66`, `SPEC.md:227`) — SPEC defines `DocumentRef` as tuple struct `(pub String)` with `"FILENAME > Section"` support. Implementation is a named-field struct `{ pub filename: String }` with no section support.
- **BootstrapError::Io vs ::Storage** (`SPEC.md:193-199`, `lib.rs:40-56`) — SPEC defines `BootstrapError` with `Io(std::io::Error)` variant. Implementation uses `Storage(String)` instead.
- **DOCUMENT_FORMAT omits related-line** (`prompts.rs:23-28`) — DOCUMENT_FORMAT prompt omits `<!-- related: ... -->` line documented in SPEC as part of standard header.

### NC3: CHANGELOG file extension (storage.rs, SPEC.md, lib.rs, bootstrap.rs)

Single rename propagates across files.

- **CHANGELOG.md misrepresents JSONL format** (`storage.rs:145-146`) — `changelog_path` returns path to `CHANGELOG.md` but the file stores JSONL, not Markdown. Should be `.jsonl` or content should be Markdown.
- **SPEC acknowledges mismatch** (`SPEC.md:84-86`) — SPEC notes JSONL in `.md` file.

### NC4: BootstrapError placement (lib.rs → bootstrap.rs)

- **Error type misplaced in facade** (`lib.rs:40-56`) — `BootstrapError` defined in `lib.rs` but exclusively produced/consumed by `bootstrap.rs`. Error type and `From` impl for an operation live in the facade, not the operation module. Will scale poorly as operations are added.

### NC5: CI pipeline consolidation (ci.yml)

All in the same file; matrix refactor addresses the first three together.

- **Missing timeout on macOS/Windows jobs** (`ci.yml:61, 74`) — `test-macos` and `test-windows` have no `timeout-minutes`, unlike `test-linux` (15 min). Hanging tests consume runner time until 6-hour GitHub default.
- **Near-identical jobs should use matrix strategy** (`ci.yml:41-91`) — Three near-identical test jobs differing only by OS. Same cache block repeated five times. Matrix strategy would remove ~30 lines.
- **Redundant build job** (`ci.yml:93-111`) — `build` job runs `cargo build` on all three OSes but clippy and test jobs already compile the code.
- **No integration or CLI smoke tests** (`ci.yml`) — No integration or end-to-end test step. `vault-cli` has zero `#[test]` functions. No CLI smoke test.

### NC6: Test coverage gaps (bootstrap.rs, storage.rs)

All require adding or improving tests in the same area.

- **Non-deterministic timestamps** (`bootstrap.rs:47-48`, `storage.rs:386-403`) — `utc_now_iso8601()` produces non-deterministic timestamps called directly rather than injected. No test verifies the `ts` field value.
- **Missing error-path coverage** (`bootstrap.rs:35-36`) — No test exercises the `BootstrapError::Storage` path for failures in `build_bootstrap_prompt` or `append_changelog`. No test for `is_initialized` when only `raw/` or only `derived/` exists independently.
- **Platform-gated test** (`storage.rs:840-882`) — `validate_derived_unreadable_file_warns_and_continues` gated on `#[cfg(unix)]`, never runs on Windows.
- **Silent parse failure in list_all_raw** (`storage.rs:371-378`) — `parse::<u32>()` failure silently discarded, file skipped.
- **MockLibrarian discards prompt/query** (`test_support.rs:47-61`) — Most operation tests use `MockLibrarian` which does not capture the prompt. Only `CapturingLibrarian` tests verify prompt content. Prompt regressions in non-capturing tests would go undetected.
- **No error-path coverage for record I/O failures** (`record.rs`) — No test exercises `RecordError::Io` path for `snapshot_derived` or `validate_derived` failures during record. Same gap as bootstrap error-path coverage above.
- **Validation warning test checks only non-emptiness** (`record.rs:387-391`) — `record_validation_warnings_do_not_fail` asserts `!warnings.is_empty()` but does not verify which warnings are produced or their content.

### NC7: Storage code cleanup (storage.rs)

Minor cleanups in the same file.

- **Redundant guard in `is_valid_raw_name`** (`storage.rs:109-111`) — `len() >= 2` check redundant; regex already requires minimum 2 characters.
- **Dead code in `write_raw_versioned`** (`storage.rs:264-268`) — `versions.last().map_or(1, ...)` fallback is dead; the `versions.is_empty()` check already returns an error, so `.last()` is guaranteed `Some`.
- **Blanket `dead_code` allow** (`storage.rs:1-4`) — Blanket `#![allow(dead_code)]` justified by "future operations" comment. Some methods are now called; replace with per-item annotations or remove.

### NC8: Unused CLI dependencies (vault-cli/Cargo.toml)

- `vault-cli` pulls in `vault`, `tokio`, `serde`, `serde_json` but `main.rs` uses none of them.

### NC9: Documentation accuracy (SPEC.md, DESIGN.md)

All documentation corrections.

- **SPEC stale references** (`SPEC.md:369, 482-483`) — Historical reference to "Python epic predecessor" violates CLAUDE.md. Pinned rev note will grow stale.
- **SPEC undefined "lot" term** (`SPEC.md:351`) — "lot" referenced without introduction; not in Sibling Projects table.
- **DESIGN inaccurate description** (`DESIGN.md:62`) — Says `produce_derived` "reads raw documents and writes derived documents" — method invokes an agent that does the reading/writing, not the method itself.
- **`RECORD_BLOCK` name doesn't signal template** (`prompts.rs:120`) — Constant requires `RAW_FILENAME_PLACEHOLDER` substitution before use, but name `RECORD_BLOCK` doesn't convey that it's a template. Compare `BOOTSTRAP_BLOCK` which needs no substitution.
- **DESIGN tokio description** (`DESIGN.md:33`) — Says "async runtime for bootstrap operation tests" — should say "operation tests" since record also uses tokio.

---

## NIT

### T1: Storage API design decisions (storage.rs)

All relate to storage API design and concurrency assumptions; address together when API solidifies.

- **TOCTOU race in `write_raw_versioned`** (`storage.rs:246-269`) — `scan_versions` then `fs::write` without locking. Spec says access is serialized; single-user CLI.
- **No file locking on changelog** (`storage.rs:177-185`) — Concurrent appends could corrupt JSONL. Same single-user constraint applies.
- **`write_raw_versioned` bool param** (`storage.rs:246`) — `new_series: bool` controls two distinct operations. Consider splitting into `create_raw_series`/`append_raw_version` when operations layer solidifies.
- **Boolean parameter and enum naming** (`storage.rs:26-27, 77-81`) — `VersionConflict` name (→ `SeriesAlreadyExists`), `DerivedValidationWarning` name. Revisit with API.

### T2: Prompts module cleanup (prompts.rs)

All in `prompts.rs`.

- **Trivial functions could be inlined** (`prompts.rs:112-114, 121-123`) — `bootstrap_system_prompt` is a one-line function; `bootstrap_query` as `const fn` vs `pub const`.
- **Untested branches** (`prompts.rs:44-69, 126-129`) — Error path and only-raw/only-derived inventory branches not tested.
- **Coupled to Storage type** (`prompts.rs:126-129`) — `build_bootstrap_prompt` takes `&Storage` but only calls `.inventory()`. Could accept `&DocumentInventory` directly.
- **writeln! result suppressed** (`prompts.rs:55, 63`) — `let _ =` on `writeln!`. Writing to String is infallible in practice.

### T3: Librarian naming and ceremony (librarian.rs)

Both in `librarian.rs`.

- **Module and trait naming mismatch** (`librarian.rs:1, 23-30`) — `LibrarianInvoker` reads as "thing that invokes a librarian" rather than "librarian abstraction." `Librarian` would align with `ReelLibrarian`.
- **Minor code ceremony** (`librarian.rs:11, 56, 65`) — `tool_grants` intermediate variable, `_result` with explicit type, `AGENT_TIMEOUT` potentially unused in-file.

### T4: Module structure and separation (storage.rs)

Code placement concerns for when the crate grows.

- **Validation and utility logic embedded in Storage** (`storage.rs:293-346, 386-420`) — `validate_derived` mixes filesystem enumeration with content-level policy checks. Timestamp/validation utilities could be extracted.
- **Hand-rolled calendar math** (`storage.rs:386-420`) — `days_to_civil` is ~13 lines of Hinnant's algorithm; `time` crate could replace it. Defensible if staying dependency-light; design doc documents this rationale.
- **`StorageError` placement** — May need extraction to `error.rs` when the operations layer introduces its own errors.

### T5: Documentation structure (SPEC.md, DESIGN.md, STATUS.md)

Organizational concerns that do not affect correctness.

- **SPEC.md** — Redundant sections (159-181, 345-358). Mixed concerns in storage/partial-failure/integration sections (54-157, 212-220, 340-392, 457-476). CLI/orchestrator details out of scope for library spec (396-453, 459-475). Error design gaps for unimplemented operations (41-47, 193-199, 249-291, 320-365). Sandbox not implemented (351-358, 481-491).
- **DESIGN.md** — Redundant content in layer diagram and key types (37-48, 83-88). Implementation-level detail misplaced (66-75, 82-88). Mixed abstraction in storage section (77-104).
- **STATUS.md** — API inventory duplicates code/SPEC; should be milestone-level (13-36).

### T6: Bootstrap test code (bootstrap.rs)

- **Mock implementations repeat code** (`bootstrap.rs:112-125, 150-185`) — Mocks repeat `.map_err` pattern. Test code; works correctly.

### T7: Standalone small items

- **CI test flags** (`ci.yml:56, 72, 91`) — `cargo test` runs without `--all-targets` and `--workspace`. Resilience concern for future config changes.
- **CLI about string** (`vault-cli/src/main.rs:7`) — `about` string is just `"Vault"` — repeats the program name. CLI is a stub.
- **bootstrap() doc comment incomplete** (`lib.rs:103-108`) — Omits post-invocation validation step and warning output.
- **validate_derived skip behavior** (`storage.rs:307-313`) — Code `continue`s on filename failure, skipping header check. Reasonable behavior.
- **Nu shell re-invocation pattern** (`vault_shell.nu:3, 5-9`) — Pattern via `^nu --env-config $self_path` followed by `exit` is a standard nushell idiom for env-config bootstrapping. The new nu process becomes the interactive session; nothing is lost.
- **Nu script description** (`vault_project_assistant.nu:7`) — Tells user "project status summary" but actual behavior is broader.
