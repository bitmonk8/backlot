//! Immutable, fully-validated workflow value.
//!
//! [`Workflow`] is the product of the loader pipeline and the primary type
//! that execution code operates on.  It is cheap to clone (single `Arc`
//! increment), `Send + Sync`, and uses [`BTreeMap`] throughout so that
//! iteration order — and therefore any debug / error output derived from it —
//! is deterministic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cel::{CelExpression, Template};
use crate::error::{MechError, MechResult};
use crate::schema::{MechDocument, SchemaRegistry};

// ---------------------------------------------------------------------------
// WorkflowInner
// ---------------------------------------------------------------------------

/// Private inner data behind [`Workflow`]'s single `Arc`.
#[derive(Debug)]
pub(crate) struct WorkflowInner {
    pub(crate) document: MechDocument,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) schemas: SchemaRegistry,
    pub(crate) cel_expressions: BTreeMap<String, Arc<CelExpression>>,
    pub(crate) templates: BTreeMap<String, Arc<Template>>,
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

/// An immutable, fully-validated workflow, ready for execution.
///
/// Produced by [`crate::load_workflow`] (or [`crate::load_workflow_str`] for
/// in-memory tests). Holds:
///
/// * The parsed and inference-resolved [`MechDocument`].
/// * A compiled [`SchemaRegistry`] covering every workflow-level shared
///   schema.
/// * A deduplicated cache of compiled CEL expressions (keyed by source
///   text) — every `when:` guard clause and every `set_context` /
///   `set_workflow` value expression in the workflow.
/// * A deduplicated cache of compiled [`Template`] strings (keyed by source
///   text) — every workflow-level `defaults.system` and function-level
///   `overrides.system` template, every block `prompt:`, every
///   top-level `input` / `output` mapping value on a call block, and
///   every per-call entry `input` mapping value on a call block.
///
/// The struct is `Send + Sync` and deliberately uses [`BTreeMap`] so that
/// iteration order — and therefore any debug / error output derived from it —
/// is deterministic.
///
/// Cheap to clone (single `Arc` increment).
#[derive(Debug, Clone)]
pub struct Workflow(pub(crate) Arc<WorkflowInner>);

impl Workflow {
    /// Construct a `Workflow` from a fully-populated inner value.
    pub(crate) fn new(inner: WorkflowInner) -> Self {
        Self(Arc::new(inner))
    }

    /// The parsed, validated, inference-resolved workflow document.
    pub fn document(&self) -> &MechDocument {
        &self.0.document
    }

    /// The path the workflow was loaded from, if any.
    pub fn source_path(&self) -> Option<&Path> {
        self.0.source_path.as_deref()
    }

    /// The compiled shared-schema registry.
    pub fn schemas(&self) -> &SchemaRegistry {
        &self.0.schemas
    }

    /// Look up a compiled CEL expression by source text.
    pub fn cel_expression(&self, source: &str) -> Option<&Arc<CelExpression>> {
        self.0.cel_expressions.get(source)
    }

    /// Look up a compiled template by source text.
    pub fn template(&self, source: &str) -> Option<&Arc<Template>> {
        self.0.templates.get(source)
    }

    /// Look up a compiled CEL expression by source text, returning a
    /// [`MechError::InternalInvariant`] when the source is not present
    /// in the loader cache. The loader is contractually required to
    /// intern every CEL expression in a validated workflow.
    pub fn require_cel(&self, source: &str) -> MechResult<&Arc<CelExpression>> {
        self.0
            .cel_expressions
            .get(source)
            .ok_or_else(|| MechError::InternalInvariant {
                message: format!(
                    "CEL expression `{source}` should have been compiled at load time"
                ),
            })
    }

    /// Same as [`Workflow::require_cel`], but for compiled templates.
    pub fn require_template(&self, source: &str) -> MechResult<&Arc<Template>> {
        self.0
            .templates
            .get(source)
            .ok_or_else(|| MechError::InternalInvariant {
                message: format!("template `{source}` should have been interned at load time"),
            })
    }

    /// Number of distinct compiled CEL expressions.
    pub fn cel_expression_count(&self) -> usize {
        self.0.cel_expressions.len()
    }

    /// Number of distinct compiled templates.
    pub fn template_count(&self) -> usize {
        self.0.templates.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_workflow_str;

    /// Minimal workflow exercising both interner caches: one transition
    /// guard CEL expression and two distinct prompt templates.
    const YAML: &str = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hello"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
        transitions:
          - when: "output.answer == \"yes\""
            goto: b
          - goto: b
      b:
        prompt: "world"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;

    const GUARD_SRC: &str = r#"output.answer == "yes""#;
    const TEMPLATE_SRC: &str = "hello";

    #[test]
    fn require_cel_returns_interned_expression() {
        let wf = load_workflow_str(YAML).expect("workflow must load");
        let expr = wf.require_cel(GUARD_SRC).expect("guard must be interned");
        // Same Arc as the Option-returning accessor.
        let direct = wf.cel_expression(GUARD_SRC).unwrap();
        assert!(Arc::ptr_eq(expr, direct));
    }

    #[test]
    fn require_cel_missing_returns_invariant_error() {
        let wf = load_workflow_str(YAML).expect("workflow must load");
        let missing = "no.such.expression";
        let err = wf
            .require_cel(missing)
            .expect_err("unknown CEL source must error");
        assert!(
            matches!(&err, MechError::InternalInvariant { message } if message.contains(missing)),
            "expected InternalInvariant mentioning `{missing}`, got {err:?}"
        );
    }

    #[test]
    fn require_template_returns_interned_template() {
        let wf = load_workflow_str(YAML).expect("workflow must load");
        let tmpl = wf
            .require_template(TEMPLATE_SRC)
            .expect("template must be interned");
        let direct = wf.template(TEMPLATE_SRC).unwrap();
        assert!(Arc::ptr_eq(tmpl, direct));
    }

    #[test]
    fn require_template_missing_returns_invariant_error() {
        let wf = load_workflow_str(YAML).expect("workflow must load");
        let missing = "never-interned-{{input.x}}";
        let err = wf
            .require_template(missing)
            .expect_err("unknown template source must error");
        assert!(
            matches!(&err, MechError::InternalInvariant { message } if message.contains(missing)),
            "expected InternalInvariant mentioning `{missing}`, got {err:?}"
        );
    }
}
