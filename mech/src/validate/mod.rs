//! Load-time validation of a parsed [`MechDocument`] (spec §10.1).
//!
//! # Ordering invariant
//!
//! This pass runs **before** function output inference. Do not read
//! resolved/inferred output schemas here — functions declaring `output: infer`
//! still have an unresolved schema at this point, and any check that peeks at
//! a concrete output shape would silently skip them. See `loader.rs`
//! `load_impl` for the ordering contract and how to add a post-inference pass
//! if one becomes necessary.
//!
//! [`validate_workflow`] walks the parsed YAML AST and emits the **complete**
//! list of errors and warnings — it never short-circuits on the first error.
//! All checks listed in `docs/MECH_SPEC.md` §10.1 are implemented here:
//! structural (block discrimination, name format, context declarations, …),
//! graph (DAG check on `depends_on`, transition target existence, dominator-
//! based template reachability, …), and type (schema validity, CEL
//! compilation + variable scope, CEL optional field safety, agent model resolution, input-schema match
//! against callee, …).
//!
//! # Hermetic agent model resolution
//!
//! The spec says `agent.model` resolves via flick's `ModelRegistry`. That is
//! a filesystem-touching operation, so this module accepts any
//! [`ModelChecker`] implementation. Two ready-made impls are provided:
//!
//! * [`AnyModel`] — accepts every model name (use in tests where model
//!   resolution is irrelevant).
//! * [`KnownModels`] — accepts only names from a fixed set.
//!
//! Production callers should pass an adapter over flick's `ModelRegistry`.

mod agents;
mod blocks;
mod cel_check;
pub(crate) mod graph;
mod helpers;
mod model;
mod report;
mod schema_check;

// Re-export the public API (must match what lib.rs re-exports).
pub use model::{AnyModel, KnownModels, ModelChecker};
pub use report::{Location, ValidationIssue, ValidationReport};

use std::collections::BTreeSet;
use std::path::Path;

use crate::schema::MechDocument;

/// Validate a parsed workflow against the §10.1 checklist.
///
/// `file_path` is folded into the source location of every emitted issue.
/// `models` is consulted for `agent.model` resolution.
pub fn validate_workflow(
    workflow: &MechDocument,
    file_path: Option<&Path>,
    models: &dyn ModelChecker,
) -> ValidationReport {
    let mut v = Validator::new(file_path);
    v.run(workflow, models);
    v.report
}

// ---- Internal validator state --------------------------------------------

pub(crate) struct Validator<'a> {
    pub(crate) file: Option<&'a Path>,
    pub(crate) report: ValidationReport,
}

impl<'a> Validator<'a> {
    fn new(file: Option<&'a Path>) -> Self {
        Self {
            file,
            report: ValidationReport::default(),
        }
    }

    pub(crate) fn err(&mut self, loc: Location, msg: impl Into<String>) {
        self.report.errors.push(ValidationIssue::new(loc, msg));
    }

    pub(crate) fn warn(&mut self, loc: Location, msg: impl Into<String>) {
        self.report.warnings.push(ValidationIssue::new(loc, msg));
    }

    pub(crate) fn root_loc(&self) -> Location {
        Location::root(self.file)
    }

    fn run(&mut self, wf: &MechDocument, models: &dyn ModelChecker) {
        // Top-level
        if wf.functions.is_empty() {
            self.err(
                self.root_loc().with_field("functions"),
                "workflow must declare at least one function",
            );
        }

        // Workflow-level context
        if let Some(defaults) = &wf.workflow {
            self.validate_context_map(
                &defaults.defaults.context,
                &self.root_loc().with_field("workflow.context"),
            );
            self.validate_named_agents(defaults, models);
            if let Some(agent_ref) = &defaults.defaults.agent {
                self.validate_agent_ref_strict(
                    agent_ref,
                    defaults,
                    models,
                    self.root_loc().with_field("workflow.agent"),
                );
            }
        }

        // Function-level
        let function_names: BTreeSet<String> = wf.functions.keys().cloned().collect();
        for (fn_name, func) in &wf.functions {
            self.validate_function(fn_name, func, wf, &function_names, models);
        }
    }
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests;
