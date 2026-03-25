# Known Issues

## Storage Layer

- **TOCTOU in `write_raw_versioned`** — `scan_versions` then `write_raw` without `O_EXCL`. Spec says access is serialized by orchestrator, so not a bug today, but could matter if concurrency model changes. (`storage.rs:263-287`)
- **`write_raw_versioned` bool param** — `new_series: bool` controls two distinct operations. Consider splitting into `create_raw_series`/`append_raw_version` when the operations layer solidifies the API surface. (`storage.rs:263`)
- **`DocumentRef` is a single-field wrapper** — Could be simplified to `String`, but matches spec's `DocumentRef(pub String)` newtype. Revisit if it stays trivial. (`storage.rs:66`)
- **Three regexes share a core pattern** — `RAW_NAME_RE`, `DERIVED_FILENAME_RE`, `RAW_VERSIONED_RE` could be reduced to one regex + string ops. Low priority. (`storage.rs:96-112`)
- **`validate_derived` aborts on unreadable file** — `fs::read_to_string` failure aborts all validation. Should warn and continue. (`storage.rs:333`)
- **`utc_now_iso8601` has no time injection seam** — Impossible to write deterministic tests for specific dates/times. Consider accepting a `Duration` parameter. (`storage.rs:395`)
- **Timestamp/validation utility placement** — `utc_now_iso8601`/`days_to_civil` and name validation could be extracted to `time.rs`/`validation.rs`. Premature at current crate size. (`storage.rs:395-428`, `96-129`)
- **`VersionConflict` naming** — `SeriesAlreadyExists` would better describe the condition. Defer to API solidification. (`storage.rs:25`)
- **`StorageError` placement** — May need extraction to `error.rs` when the operations layer introduces its own errors. (`storage.rs:17`)
- **Missing test coverage** — `read_raw` not-found error path, `read_changelog` with corrupt data, `list_derived` direct tests, `list_all_raw` with non-matching files, `create_directories` failure cases. (`storage.rs`)
- **`validate_derived` single-warning for empty files** — Empty file missing both title and scope only reports the title warning due to `else if` structure. (`storage.rs:342-353`)
- **Redundant `len >= 2` check in `is_valid_raw_name`** — The regex already requires minimum 2 characters. (`storage.rs:117`)
- **CHANGELOG.md extension mismatch** — File contains JSONL data but uses `.md` extension. Consider `.jsonl` or documenting the rationale. (`storage.rs:152-154`)
- **Derived document content validation in Storage** — `validate_derived` mixes filesystem enumeration with content-level policy checks. Consider extracting content validation when the operations layer lands. (`storage.rs:310-357`)
- **`DocumentRef` naming** — `Ref` suffix conventionally implies borrowing in Rust. Consider `DocumentEntry` or `DocumentName`. (`storage.rs:65-68`)
- **Hand-rolled calendar math** — `days_to_civil` is 35 lines of calendar arithmetic; the `time` crate could replace it. Defensible if staying dependency-light. (`storage.rs:416-428`)
