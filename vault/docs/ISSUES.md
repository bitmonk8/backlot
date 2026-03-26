# Known Issues

**Severity scale:** MUST-FIX (functional impact or spec contract violation, fix before ship) · NON-CRITICAL (noticeable spec/impl mismatch, functionally acceptable) · NIT (cosmetic divergence)

## NON-CRITICAL

#### 1: Bootstrap CLI emits no JSON to stdout on success

**Status:** Confirmed · **Recommendation:** Fix implementation

The spec states "All subcommands emit JSON to stdout on success" (`SPEC.md:451`). The `query`, `record`, and `reorganize` subcommands all serialize their results as JSON to stdout. The `bootstrap` subcommand emits nothing to stdout — it only emits validation warnings to stderr.

The library returns `Vec<DerivedValidationWarning>` which could be serialized as JSON, or an empty `[]` when there are no warnings. The bootstrap-specific CLI section (`SPEC.md:417-423`) does not explicitly mention JSON output, unlike the other three operations, so there is an internal inconsistency in the spec. Regardless, the blanket "all subcommands" statement is clear.

**Fix:** Serialize the `Vec<DerivedValidationWarning>` as JSON to stdout in `run_bootstrap`, matching the pattern used by the other three subcommands. This satisfies the spec's blanket requirement and gives callers a machine-readable success signal.

**Severity assessment:** NON-CRITICAL is correct. This is a spec contract violation (the blanket "all subcommands" statement), but bootstrap is a one-shot operation and callers can infer success from exit code 0. No functional impact.

- **Spec:** "All subcommands emit JSON to stdout on success." (`SPEC.md:451`)
- **Impl:** `run_bootstrap` returns `Ok(())` with no stdout output (`vault-cli/src/main.rs:173-183`)
- **Other subcommands:** All three emit `println!("{json}")` (`main.rs:194, 216, 226`)

#### 2: Typed section vocabulary omitted from librarian prompt

**Status:** Confirmed · **Recommendation:** Fix implementation

The spec defines six standard section types for derived documents: `Decisions`, `Constraints`, `Open Questions`, `Approach`, `Findings`, `Interfaces` (`SPEC.md:134-145`). Each has a defined semantic purpose (e.g., "Resolved choices with rationale" for Decisions). The `DOCUMENT_FORMAT` prompt block tells the librarian only "Sections: Use `##` headings. Each section has a clear purpose." without enumerating these types.

Without this vocabulary, the librarian invents ad-hoc section names across invocations. This reduces consistency and makes it harder for consumers to locate specific information types across documents. The spec notes "Not every document needs all section types" — these are guidelines, not hard requirements — but the librarian currently has no awareness of them at all.

**Fix:** Add the six typed section names and their descriptions to the `DOCUMENT_FORMAT` prompt block, with a note that not every document needs all types. This gives the librarian a shared vocabulary without making the types mandatory.

**Severity assessment:** NON-CRITICAL is correct. The librarian still produces functional sections — they just lack consistency across invocations. No data loss or functional breakage.

- **Spec:** Six named section types with descriptions (`SPEC.md:134-145`)
- **Impl:** "Sections: Use `##` headings. Each section has a clear purpose." (`prompts.rs:27`)

## NIT

#### 3: `<!-- related: ... -->` header not included in prompt

**Status:** Confirmed · **Recommendation:** Fix spec (remove `<!-- related: -->` from standard header)

The spec's standard header for derived documents includes three lines: title, scope comment, and related comment (`SPEC.md:127-130`). The `DOCUMENT_FORMAT` prompt block instructs the librarian to produce only the title and scope lines (`prompts.rs:26`). The related comment (`<!-- related: OTHER_DOC.md, ANOTHER_DOC.md -->`) is not mentioned.

The cross-references prompt block (`prompts.rs:30-34`) covers the same functional goal via inline markdown links (`See [DESIGN.md](DESIGN.md)`), and the post-invocation validation does not check for the related header (`SPEC.md:362-364` only validates title and scope). The divergence is cosmetic: derived documents will use inline cross-references instead of the structured `<!-- related: -->` metadata.

**Fix:** Remove the `<!-- related: ... -->` line from the spec's standard header definition. The implementation's approach — inline markdown links — is superior: links are visible in rendered markdown, discoverable in context, and enforceable by the existing cross-references prompt block. The `<!-- related: -->` comment is invisible in rendered output, not validated, and redundant with inline links. The spec should match the implementation here.

**Severity assessment:** NIT is correct. Cosmetic divergence with no functional impact. The cross-referencing goal is met via a different (arguably better) mechanism.

- **Spec:** Standard header includes `<!-- related: OTHER_DOC.md, ANOTHER_DOC.md -->` (`SPEC.md:130`)
- **Impl:** Header guidance is title + scope only (`prompts.rs:26`); cross-references via inline links (`prompts.rs:30-34`)
