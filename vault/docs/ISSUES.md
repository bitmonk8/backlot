# Known Issues

**Severity scale:** MUST-FIX (functional impact, fix before ship) · NON-CRITICAL (spec/impl mismatch, no functional harm) · NIT (cosmetic or stylistic)

## NON-CRITICAL

#### 7: `DocumentRef` inline comment in SPEC.md claims section-qualified format

The SPEC.md comment on `DocumentRef` (line 227) says `// "FILENAME" or "FILENAME > Section" format`. The implementation in `storage.rs` only stores bare filenames (`doc.filename` is documented as "filename only, no directory prefix") and never uses the `"FILENAME > Section"` format. Either the comment should be narrowed to match the implementation, or the implementation should be extended to support section-qualified references.

- **Spec:** `pub struct DocumentRef { pub filename: String } // "FILENAME" or "FILENAME > Section" format` (`SPEC.md:227`)
- **Impl:** `pub struct DocumentRef { pub filename: String }` with doc comment "filename only, no directory prefix" (`storage.rs:61-64`)

**Review verdict:** Real issue. Severity correct (NON-CRITICAL).

**Resolution:** Fix the spec. The `"FILENAME > Section"` format is aspirational — no code parses it, the query prompt explicitly instructs `"source": "<filename>"` with bare filenames (`prompts.rs:190,196`), and `DocumentRef` is also used for changelog entries and reorganize reports where section granularity is meaningless. If section-level extract references are needed in the future, they should be a separate field on `Extract`, not an overloaded `DocumentRef.filename`. Remove the `// "FILENAME" or "FILENAME > Section" format` comment from SPEC.md line 227.
