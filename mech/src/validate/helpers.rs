//! Small utility functions and constants used across validate submodules.

use std::collections::BTreeSet;

use crate::schema::{BlockDef, CallBlock, CallSpec, FunctionDef};

// ---- Identifiers ----------------------------------------------------------

/// Returns `true` if `s` matches the identifier pattern `[a-z][a-z0-9_]*`.
pub(crate) fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// ---- Block utilities ------------------------------------------------------

/// Extract the function names referenced by a call block (all three forms).
pub(crate) fn called_function_names(c: &CallBlock) -> Vec<String> {
    match &c.call {
        CallSpec::Single(name) => vec![name.clone()],
        CallSpec::Uniform(names) => names.clone(),
        CallSpec::PerCall(entries) => entries.iter().map(|e| e.func.clone()).collect(),
    }
}

/// Return the set_context and set_workflow keys written by a block.
pub(crate) fn block_writes(b: &BlockDef) -> (BTreeSet<&str>, BTreeSet<&str>) {
    (
        b.set_context().keys().map(String::as_str).collect(),
        b.set_workflow().keys().map(String::as_str).collect(),
    )
}

/// Infer terminal blocks: those with no outgoing transitions.
pub(crate) fn inferred_terminals(func: &FunctionDef) -> Vec<String> {
    func.blocks
        .iter()
        .filter(|(_, b)| b.transitions().is_empty())
        .map(|(name, _)| name.clone())
        .collect()
}

// ---- Constants ------------------------------------------------------------

// Reserved names cover two distinct concerns:
//   1. The 6 CEL namespaces bound by `cel::Namespaces::to_context`
//      (`input`, `context`, `workflow`, `block`, `blocks`, `meta`). A block
//      named after any of them silently shadows the runtime namespace in
//      transition guards and templates.
//   2. The synthetic `output` field, an extra variable bound by
//      `build_post_block_namespaces` for transition-guard / set_context /
//      set_workflow expressions.
// The reserved list MUST cover all 7 names (6 namespaces + `output`).
pub(crate) const RESERVED_BLOCK_NAMES: &[&str] = &[
    "input", "output", "context", "workflow", "block", "blocks", "meta",
];
pub(crate) const VALID_GRANTS: &[&str] = &["tools", "write", "network"];
pub(crate) const VALID_JSON_TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "array", "object", "null",
];
pub(crate) const ALLOWED_NAMESPACES: &[&str] =
    &["input", "output", "context", "workflow", "blocks", "block"];
