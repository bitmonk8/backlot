//! Function execution-mode classification.
//!
//! A mech function runs in one of two execution modes, derived from the edges
//! between its blocks:
//!
//! * **Imperative (CFG)** — at least one block declares outgoing
//!   `transitions:`. The function executes as a control-flow graph: blocks
//!   are visited one at a time along the transition edges.
//! * **Dataflow** — no block declares `transitions:` and at least one block
//!   declares `depends_on:`. The function executes as a DAG: each block runs
//!   when all its inputs are ready, possibly in parallel.
//!
//! A function with neither `transitions:` nor `depends_on:` (a single block,
//! or several disconnected blocks) classifies as **Imperative** by default.
//!
//! This classification is consumed by:
//!
//! * [`crate::schema::infer`] — to choose the multi-terminal output schema
//!   shape (keyed map for dataflow, structural unification for imperative).
//! * [`crate::loader`] — to emit the load-time
//!   [`LoadWarning::CompactionOnDataflowFunction`] advisory.
//!
//! It mirrors `exec::function::detect_mode` without depending on the exec
//! layer, so loader/schema callers stay layer-clean. (Unifying the two copies
//! is tracked as a follow-up — see `r2_triage.md` R2-T8.)
//!
//! [`LoadWarning::CompactionOnDataflowFunction`]: crate::loader::LoadWarning::CompactionOnDataflowFunction

use crate::schema::FunctionDef;

/// Execution mode detected from a function's block edges.
#[derive(PartialEq)]
pub(crate) enum InferMode {
    /// Any block has outgoing transitions → imperative (CFG) mode.
    Imperative,
    /// No block has transitions, at least one has `depends_on` → dataflow mode.
    Dataflow,
}

/// Detect execution mode from a function's block edges (mirrors
/// `exec::function::detect_mode` without importing from the exec layer).
pub(crate) fn infer_mode(func: &FunctionDef) -> InferMode {
    let has_transitions = func.blocks.values().any(|b| !b.transitions().is_empty());
    if has_transitions {
        return InferMode::Imperative;
    }
    let has_depends = func.blocks.values().any(|b| !b.depends_on().is_empty());
    if has_depends {
        InferMode::Dataflow
    } else {
        InferMode::Imperative
    }
}
