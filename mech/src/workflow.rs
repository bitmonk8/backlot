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
/// * A deduplicated cache of compiled CEL expressions (keyed by
///   source text) — every `when:` clause in the workflow.
/// * A deduplicated cache of compiled [`Template`] strings (keyed by source
///   text) — every `prompt:`, `set_context` / `set_workflow` value, and every
///   `input` / `output` mapping value on a call block.
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

    /// Number of distinct compiled CEL expressions.
    pub fn cel_expression_count(&self) -> usize {
        self.0.cel_expressions.len()
    }

    /// Number of distinct compiled templates.
    pub fn template_count(&self) -> usize {
        self.0.templates.len()
    }
}
