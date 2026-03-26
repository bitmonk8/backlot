# Known Issues

**Severity scale:** MUST-FIX (functional impact, fix before ship) · NON-CRITICAL (spec/impl mismatch, no functional harm) · NIT (cosmetic or stylistic)

## NON-CRITICAL

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
