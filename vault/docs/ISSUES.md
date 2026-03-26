# Known Issues

**Severity scale:** MUST-FIX (functional impact or spec contract violation, fix before ship) · NON-CRITICAL (noticeable spec/impl mismatch, functionally acceptable) · NIT (cosmetic divergence)

## NON-CRITICAL

#### 1: Scope restriction is not a single shared block

**Status:** Confirmed · **Recommendation:** Fix spec

The spec lists five shared blocks included in every operation's prompt and states they are "defined once as embedded constants (e.g., `include_str!`)" (`SPEC.md:389`). Scope restriction is one of these five shared blocks (`SPEC.md:377`).

The implementation defines two separate constants: `SCOPE_RESTRICTION` (used by bootstrap, record, reorganize) and `QUERY_SCOPE_RESTRICTION` (used by query). The `build_shared_prompt` function accepts `scope_restriction: &str` as a parameter, breaking the "defined once" model.

The spec's intended mechanism is: the shared scope restriction says "Read-write `derived/`" and the query operation-specific block says "No writes" (`SPEC.md:384`). The implementation instead replaces the shared block entirely for query with a read-only variant. The result is functionally identical (query cannot write) but the composition mechanism differs.

The implementation's approach is the better design. Relying on an LLM to correctly reconcile conflicting instructions ("Read-write derived/" in shared block vs "No writes" in operation block) is fragile. Providing a single unambiguous scope restriction per operation is more reliable. The spec should be updated to reflect that scope restriction is parameterized per operation, not a single shared constant.

- **Spec:** Five shared blocks defined once as embedded constants, scope restriction is shared (`SPEC.md:370-389`)
- **Impl:** `SCOPE_RESTRICTION` and `QUERY_SCOPE_RESTRICTION` as separate constants, `build_shared_prompt` takes scope as parameter (`prompts.rs:42-48, 81, 161-166`)

## NIT

#### 2: Bootstrap prompt hardcodes raw requirements path

**Status:** Confirmed · **Recommendation:** Fix spec

The spec says the bootstrap-specific prompt block has "Raw requirements path provided" (`SPEC.md:387`), suggesting the path is supplied dynamically.

The implementation hardcodes `raw/REQUIREMENTS_1.md` in `BOOTSTRAP_BLOCK` (`prompts.rs:97`). The `bootstrap_system_prompt` function takes only `&DocumentInventory` and does not accept a raw filename parameter.

This is always correct in practice because bootstrap always writes `REQUIREMENTS_1.md`. The document inventory block (generated dynamically) also lists the actual file. The spec should say the path is known at compile time, not "provided."

- **Spec:** "Raw requirements path provided" (`SPEC.md:387`)
- **Impl:** Path hardcoded in `BOOTSTRAP_BLOCK` (`prompts.rs:97`)

#### 3: Query response parser allows missing extracts field

**Status:** Confirmed · **Recommendation:** Fix spec

The query prompt requires a JSON object with an `extracts` array (`prompts.rs:195`). The parser in `parse_query_response` defaults a missing `extracts` key to an empty `Vec` rather than returning an error (`librarian.rs:168-169`).

The spec defines `QueryResult` with `extracts: Vec<Extract>` as a field (`SPEC.md:241`), but doesn't specify whether the field is required in the LLM's JSON response. An empty Vec is a valid value. The parser's leniency is intentional — LLMs omit fields. The spec should note that `extracts` may be absent (treated as empty).

- **Spec:** `extracts: Vec<Extract>` in `QueryResult` struct (`SPEC.md:241`)
- **Impl:** Missing `extracts` defaults to empty Vec instead of error (`librarian.rs:168-169`)
