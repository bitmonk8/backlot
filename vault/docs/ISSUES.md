# Known Issues

## Storage Layer

### NON-CRITICAL

#### G1: `validate_derived` logic bugs
Both are bugs in the same function with the same root cause pattern (incomplete error/warning reporting).

- **`validate_derived` aborts on unreadable file** — `fs::read_to_string` failure propagates via `?` and aborts validation for all remaining files. Should warn and continue, consistent with the function's contract of returning warnings. (`storage.rs:315`)
- **`validate_derived` single-warning for empty files** — Empty file missing both title and scope only reports the title warning due to `else if` structure. (`storage.rs:320-331`)

#### G2: Missing test coverage
Standalone — covers multiple functions but is a single unit of work.

- **Missing test coverage** — `read_raw` not-found error path, `read_changelog` with corrupt data, `list_derived` direct tests, `list_all_raw` with non-matching files, `create_directories` failure cases. (`storage.rs`)

### NIT

#### G3: `write_raw_versioned` API design
Both concern the same function's interface and would be addressed together during operations layer API design.

- **`write_raw_versioned` bool param** — `new_series: bool` controls two distinct operations. Consider splitting into `create_raw_series`/`append_raw_version` when the operations layer solidifies the API surface. (`storage.rs:245`)
- **TOCTOU in `write_raw_versioned`** — `scan_versions` then `write_raw` without `O_EXCL`. Spec says access is serialized by orchestrator, so not a bug today, but could matter if concurrency model changes. (`storage.rs:245-269`)

#### G4: Naming and type design
All are naming/type clarity issues that should be revisited together when the API solidifies.

- **`DocumentRef` design** — Single-field wrapper (`filename: String`) that could be simplified to `String`, but matches spec's newtype pattern. Also, `Ref` suffix conventionally implies borrowing in Rust; `DocumentEntry` or `DocumentName` would be clearer. Revisit when API solidifies. (`storage.rs:62-65`)
- **`VersionConflict` naming** — `SeriesAlreadyExists` would better describe the condition. Defer to API solidification. (`storage.rs:25`)

#### G5: Module structure and separation of concerns
All concern code placement — what belongs where as the crate grows.

- **`StorageError` placement** — May need extraction to `error.rs` when the operations layer introduces its own errors. (`storage.rs:17`)
- **Derived document content validation in Storage** — `validate_derived` mixes filesystem enumeration with content-level policy checks. Consider extracting content validation when the operations layer lands. (`storage.rs:292-335`)
- **Timestamp/validation utility placement** — `utc_now_iso8601`/`days_to_civil` and name validation could be extracted to `time.rs`/`validation.rs`. Premature at current crate size. (`storage.rs:373-406`, `93-121`)

#### G6: Validation code cleanup
Both are minor cleanup in the name validation area.

- **Redundant `len >= 2` check in `is_valid_raw_name`** — The regex already requires minimum 2 characters. (`storage.rs:109`)
- **Three regexes share a core pattern** — `RAW_NAME_RE`, `DERIVED_FILENAME_RE`, `RAW_VERSIONED_RE` could be reduced to one regex + string ops. Low priority. (`storage.rs:93-104`)

#### G7: Timestamp testability
Related concerns about the time utility, but low impact.

- **`utc_now_iso8601` has no time injection seam** — Impossible to write deterministic tests for specific dates/times. Consider accepting a `Duration` parameter. (`storage.rs:373`)
- **Hand-rolled calendar math** — `days_to_civil` is ~13 lines of Hinnant's civil calendar algorithm; the `time` crate could replace it. Defensible if staying dependency-light, and the design doc documents this rationale. (`storage.rs:394-406`)
