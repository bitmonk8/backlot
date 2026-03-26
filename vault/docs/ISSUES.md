# Known Issues

**Severity scale:** MUST-FIX (functional impact, fix before ship) · NON-CRITICAL (spec/impl mismatch, no functional harm) · NIT (cosmetic or stylistic)

## MUST-FIX

#### 1: Document inventory block omits scope comments for derived documents

**Status:** Real divergence. **Resolution: Fix implementation.**

The spec requires the inventory to list "derived/ documents with their scope comments" (SPEC.md:379). The implementation lists filenames only. This matters: the scope comment tells the librarian what each document covers, which directly affects placement decisions during record and reorganize. Without it, the librarian must read each document to understand its purpose, wasting tokens and risking mis-placement.

The fix is straightforward: when building the inventory block, read the second line of each derived document (the `<!-- scope: ... -->` comment) and include it in the listing.

- **Spec:** "derived/ documents with their scope comments, raw/ documents listed by filename only" (`SPEC.md:379`)
- **Impl:** Lists `- derived/FILENAME.md` with no scope text (`prompts.rs:60-65`)

## NON-CRITICAL

#### 2: `BootstrapError` uses `Storage(String)` instead of `Io(std::io::Error)`

**Status:** Real divergence, but scoped to `BootstrapError` only. **Resolution: Fix implementation.**

The spec defines `Io(std::io::Error)` for `BootstrapError`. The implementation uses `Storage(String)` instead. However, `RecordError`, `QueryError`, and `ReorganizeError` all use `Io(std::io::Error)` matching the spec — only `BootstrapError` diverges.

The original analysis claimed "all four error enums use `Storage(String)` consistently" — this is incorrect. `RecordError` maps `StorageError` variants individually (preserving `InvalidName`, `VersionConflict`, `DocumentNotFound`, and `Io`). `QueryError` and `ReorganizeError` use `Io(#[from] std::io::Error)` directly, with `From<StorageError>` wrapping via `std::io::Error::other()`.

`BootstrapError` should be updated to use `Io(std::io::Error)` for consistency with the other three error enums and the spec. The `From<StorageError>` impl can wrap via `std::io::Error::other()`, matching the `ReorganizeError` pattern.

- **Spec:** `Io(std::io::Error)` variant (`SPEC.md:197`)
- **Impl:** `Storage(String)` variant (`lib.rs:59-60`), `From<StorageError>` converts via `.to_string()` (`lib.rs:66-70`)
- **RecordError, QueryError, ReorganizeError:** All use `Io(std::io::Error)`, matching spec (`lib.rs:92-93, 138-139, 147-148`)

#### 3: `bootstrap()` return type diverges from spec

**Status:** Real divergence. **Resolution: Update spec.**

The spec says `Result<(), BootstrapError>`. The implementation returns `Result<Vec<DerivedValidationWarning>, BootstrapError>`. The spec itself (SPEC.md:360-365) mandates post-invocation validation of derived documents with warnings that "do not fail the operation." The spec provides no mechanism to surface those warnings. The implementation solves this by returning them in the `Ok` variant. This is the correct design — warnings that are produced but never surfaced are useless.

- **Spec:** `-> Result<(), BootstrapError>` (`SPEC.md:192`)
- **Impl:** `-> Result<Vec<DerivedValidationWarning>, BootstrapError>` (`lib.rs:213-216`)

#### 4: `record()` return type diverges from spec

**Status:** Real divergence. **Resolution: Update spec.**

Same reasoning as issue 3. The spec defines `Result<Vec<DocumentRef>, RecordError>`. The implementation wraps it as `Result<(Vec<DocumentRef>, Vec<DerivedValidationWarning>), RecordError>`. Post-invocation validation produces warnings; they must be surfaced. Consistent with issues 3 and 5.

- **Spec:** `-> Result<Vec<DocumentRef>, RecordError>` (`SPEC.md:280`)
- **Impl:** `-> Result<(Vec<DocumentRef>, Vec<DerivedValidationWarning>), RecordError>` (`lib.rs:230-235`)

#### 5: `reorganize()` return type diverges from spec

**Status:** Real divergence. **Resolution: Update spec.**

Same pattern as issues 3/4. Warnings must be surfaced. Consistent across all three write operations.

- **Spec:** `-> Result<ReorganizeReport, ReorganizeError>` (`SPEC.md:320`)
- **Impl:** `-> Result<(ReorganizeReport, Vec<DerivedValidationWarning>), ReorganizeError>` (`lib.rs:263-265`)

## NIT

#### 6: `DocumentRef` is a named struct, not a tuple struct

**Status:** Real divergence. **Resolution: Update spec.**

The spec defines `pub struct DocumentRef(pub String)`. The implementation uses `pub struct DocumentRef { pub filename: String }`. The named struct is superior:

- `doc.filename` is self-documenting; `doc.0` is not.
- Serde serializes it as `{"filename": "X.md"}` which is readable JSON. A tuple struct serializes as `"X.md"` — less structured for CLI output.
- Adding fields later (if needed) doesn't break the access pattern.

- **Spec:** `pub struct DocumentRef(pub String)` (`SPEC.md:227`)
- **Impl:** `pub struct DocumentRef { pub filename: String }` (`storage.rs:61-64`)
