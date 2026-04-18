//! End-to-end workflow loader (spec §13, Deliverable 7).
//!
//! Composes parse → resolve schemas → validate → infer function outputs →
//! compile CEL expressions/templates into an immutable, ready-to-run
//! [`Workflow`] value.
//!
//! No execution logic lives here — that arrives in later deliverables. The
//! loader's job is to make sure that by the time a [`Workflow`] exists, every
//! load-time check from the spec has succeeded and every CEL expression and
//! template has been compiled exactly once.
//!
//! # Quick-start
//!
//! ```rust,ignore
//! use mech::{load_workflow, load_workflow_str};
//!
//! // From disk
//! let wf = load_workflow("path/to/workflow.yaml")?;
//!
//! // From a YAML string
//! let wf = load_workflow_str(yaml)?;
//! ```
//!
//! Load-time warnings (non-fatal advisories — see the [`LoadWarning`]
//! enum for all variants, currently [`LoadWarning::CompactionPlaceholder`]
//! and [`LoadWarning::CompactionOnDataflowFunction`]) are emitted via
//! `tracing::warn!` for production observability — callers must install a
//! `tracing` subscriber to see them. Tests use [`collect_load_warnings`]
//! against a parsed document to inspect the same advisories
//! programmatically.
//!
//! The legacy [`WorkflowLoader`] struct is still available but new code should
//! prefer the free functions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cel::{CelExpression, Template};
use crate::error::{MechError, MechResult};
use crate::schema::{
    CelSourceKind, FunctionDef, MechDocument, SchemaRegistry, infer_function_outputs,
    parse_workflow,
};
use crate::validate::{AnyModel, ModelChecker, validate_workflow};
use crate::workflow::{Workflow, WorkflowInner};

// ---------------------------------------------------------------------------
// Free-function API
// ---------------------------------------------------------------------------

/// A non-fatal advisory emitted during workflow load.
///
/// Surfaced via `tracing::warn!` for production observability — callers
/// must install a `tracing` subscriber to see them. Tests inspect the
/// same advisories programmatically via [`collect_load_warnings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadWarning {
    /// Workflow- or function-level `compaction:` is configured but the
    /// runtime compaction strategy is a placeholder no-op. The declared
    /// scope (`"workflow-level"` or `"function-level: <name>"`) is
    /// included for diagnostics.
    CompactionPlaceholder { scope: String },

    /// A dataflow function has an effective `compaction:` config (declared
    /// on the function itself or inherited from the workflow-level
    /// default), but dataflow blocks are single-turn (§4.6 rule 3): each
    /// per-block conversation is constructed empty and discarded after one
    /// prompt+response. Compaction is therefore meaningless on dataflow
    /// functions and is silently dropped at runtime. The named function is
    /// the dataflow function whose compaction config is being ignored.
    CompactionOnDataflowFunction { function: String },
}

impl std::fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompactionPlaceholder { scope } => write!(
                f,
                "{scope} `compaction` is configured but compaction is not implemented (placeholder). The hook only increments a counter; messages are NOT summarized. See docs/MECH_SPEC.md §4.6."
            ),
            Self::CompactionOnDataflowFunction { function } => write!(
                f,
                "function `{function}` is dataflow (no transitions, only `depends_on` edges) but has an effective `compaction:` config; compaction is meaningless for dataflow functions because each block runs a fresh single-turn conversation (§4.6 rule 3) and the config is dropped at runtime. See docs/MECH_SPEC.md §4.6."
            ),
        }
    }
}

/// Load, parse, and validate a workflow from disk.
///
/// Pipeline: read file → parse YAML → build schema registry → validate →
/// infer function outputs → compile CEL expressions and templates.
///
/// Load-time advisories are emitted via `tracing::warn!`; install a
/// `tracing` subscriber to capture them.
pub fn load_workflow(path: impl AsRef<Path>) -> MechResult<Workflow> {
    load_workflow_with(path, &AnyModel)
}

/// Load a workflow from disk with a custom model checker.
pub fn load_workflow_with(
    path: impl AsRef<Path>,
    models: &dyn ModelChecker,
) -> MechResult<Workflow> {
    let path = path.as_ref();
    let source = std::fs::read_to_string(path).map_err(|e| MechError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    load_impl(&source, Some(path.to_path_buf()), models).map(|(wf, _)| wf)
}

/// Load a workflow from a YAML string.
pub fn load_workflow_str(yaml: &str) -> MechResult<Workflow> {
    load_impl(yaml, None, &AnyModel).map(|(wf, _)| wf)
}

/// Load a workflow from a YAML string with a custom model checker.
pub fn load_workflow_str_with(yaml: &str, models: &dyn ModelChecker) -> MechResult<Workflow> {
    load_impl(yaml, None, models).map(|(wf, _)| wf)
}

// ---------------------------------------------------------------------------
// Legacy WorkflowLoader (kept for backward compat with mech-cli)
// ---------------------------------------------------------------------------

/// Workflow loader (delegates to [`load_workflow`] / [`load_workflow_str`]).
///
/// For new code, prefer the free functions directly.
pub struct WorkflowLoader;

impl Default for WorkflowLoader {
    fn default() -> Self {
        Self
    }
}

impl std::fmt::Debug for WorkflowLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowLoader").finish()
    }
}

impl WorkflowLoader {
    /// Create a loader with the default (`AnyModel`) model checker.
    pub fn new() -> Self {
        Self
    }

    /// Load, parse, and validate a workflow from disk.
    pub fn load(&self, path: impl AsRef<Path>) -> MechResult<Workflow> {
        load_workflow(path)
    }

    /// Load a workflow directly from a YAML string.
    pub fn load_str(&self, yaml: &str) -> MechResult<Workflow> {
        load_workflow_str(yaml)
    }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

fn load_impl(
    yaml: &str,
    source_path: Option<PathBuf>,
    models: &dyn ModelChecker,
) -> MechResult<(Workflow, Vec<LoadWarning>)> {
    // 1. Parse YAML.
    let mut file = parse_workflow(yaml).map_err(|e| MechError::YamlParse {
        path: source_path.clone(),
        message: e.to_string(),
    })?;

    // 2. Build the workflow-level shared schema registry (resolves top-level
    //    $ref-only documents, compiles every shared schema).
    let empty_schemas = BTreeMap::new();
    let schemas_map = file
        .workflow
        .as_ref()
        .map(|w| &w.schemas)
        .unwrap_or(&empty_schemas);
    let registry = SchemaRegistry::build(schemas_map)?;

    // 3. Run the §10.1 load-time validation pass. Errors → `MechError::WorkflowValidation`.
    let report = validate_workflow(&file, source_path.as_deref(), models);
    if !report.is_ok() {
        return Err(MechError::WorkflowValidation {
            errors: report.errors.iter().map(|i| i.to_string()).collect(),
        });
    }

    // Ordering invariant:
    //
    // Validation (step 3) runs BEFORE inference (step 4). Therefore any
    // rule added to `validate.rs` MUST NOT inspect concrete / resolved
    // function output schemas — functions that declare `output: infer`
    // (or omit `output:` entirely) still have an unresolved schema at
    // validation time, and such a rule would silently skip them.
    //
    // A validator that legitimately needs output-shape information must
    // either (a) work off the declared schema only, or (b) be split into
    // a post-inference pass — at which point a second `validate_workflow`
    // call needs to be added here, after `infer_function_outputs`, with a
    // documented contract about which errors are authoritative.

    // 4. Infer function output schemas (`output: infer` / omitted).
    infer_function_outputs(&mut file)?;

    // The spec describes LLM-based summarization at a token budget; today
    // the conversation compaction hook is a placeholder no-op that only
    // increments a counter. Mirror compaction-related advisories through
    // `tracing::warn!` for production observability. Tests inspect the
    // same advisories via `collect_load_warnings` against the parsed
    // document. (See `Conversation::check_compaction` and
    // `docs/MECH_SPEC.md` §4.6.)
    let warnings = collect_load_warnings(&file);
    let path_label = source_path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<inline>".to_string());
    for w in &warnings {
        match w {
            LoadWarning::CompactionPlaceholder { scope } => {
                tracing::warn!(workflow = %path_label, scope = %scope, "{w}");
            }
            LoadWarning::CompactionOnDataflowFunction { function } => {
                tracing::warn!(workflow = %path_label, function = %function, "{w}");
            }
        }
    }

    // 5. Compile every CEL guard and template in the workflow.
    let mut cel_expressions: BTreeMap<String, Arc<CelExpression>> = BTreeMap::new();
    let mut templates: BTreeMap<String, Arc<Template>> = BTreeMap::new();
    compile_all(&file, &mut cel_expressions, &mut templates)?;

    let workflow = Workflow::new(WorkflowInner {
        document: file,
        source_path,
        schemas: registry,
        cel_expressions,
        templates,
    });
    Ok((workflow, warnings))
}

fn compile_all(
    file: &MechDocument,
    cel_expressions: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    if let Some(system) = file.workflow.as_ref().and_then(|w| w.system.as_ref()) {
        intern_template(system, templates)?;
    }
    for func in file.functions.values() {
        if let Some(system) = &func.system {
            intern_template(system, templates)?;
        }
        compile_function(func, cel_expressions, templates)?;
    }
    Ok(())
}

fn compile_function(
    func: &FunctionDef,
    cel_expressions: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    for block in func.blocks.values() {
        let mut first_err: Option<MechError> = None;
        block.visit_cel_sources(&mut |source, kind| {
            if first_err.is_some() {
                return;
            }
            let result = match kind {
                CelSourceKind::Guard => intern_cel_expression(source, cel_expressions),
                CelSourceKind::Template => intern_template(source, templates),
            };
            if let Err(e) = result {
                first_err = Some(e);
            }
        });
        if let Some(e) = first_err {
            return Err(e);
        }
    }
    Ok(())
}

fn intern_cel_expression(
    source: &str,
    cel_expressions: &mut BTreeMap<String, Arc<CelExpression>>,
) -> MechResult<()> {
    if cel_expressions.contains_key(source) {
        return Ok(());
    }
    let compiled = CelExpression::compile(source)?;
    cel_expressions.insert(source.to_string(), Arc::new(compiled));
    Ok(())
}

fn intern_template(
    source: &str,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    if templates.contains_key(source) {
        return Ok(());
    }
    let compiled = Template::compile(source)?;
    templates.insert(source.to_string(), Arc::new(compiled));
    Ok(())
}

pub fn collect_load_warnings(file: &MechDocument) -> Vec<LoadWarning> {
    let mut warnings = Vec::new();
    if let Some(workflow) = &file.workflow {
        if workflow.compaction.is_some() {
            warnings.push(LoadWarning::CompactionPlaceholder {
                scope: "workflow-level".to_string(),
            });
        }
    }
    for (name, func) in &file.functions {
        if func.compaction.is_some() {
            warnings.push(LoadWarning::CompactionPlaceholder {
                scope: format!("function-level `{name}`"),
            });
        }

        // CompactionOnDataflowFunction is orthogonal to the placeholder
        // warning: the placeholder warns that compaction is a global no-op
        // today; this warning warns that even when compaction is fully
        // implemented, the config will still be silently discarded for
        // dataflow functions because dataflow blocks construct a fresh
        // single-turn `Conversation::new(None)` per block (§4.6 rule 3) and
        // never see the function-level conversation. Both warnings can fire
        // for the same function. Inheritance via workflow-level default is
        // mirrored here as a pure schema check (function-level overrides,
        // workflow-level fallback) so the loader does not depend on the
        // exec or conversation layers -- a dataflow function with no
        // function-level compaction but a workflow-level default still
        // warns, because that default would also be silently dropped at
        // runtime.
        let has_effective_compaction = func.compaction.is_some()
            || file
                .workflow
                .as_ref()
                .and_then(|w| w.compaction.as_ref())
                .is_some();
        if has_effective_compaction
            && crate::schema::infer_mode(func) == crate::schema::InferMode::Dataflow
        {
            warnings.push(LoadWarning::CompactionOnDataflowFunction {
                function: name.clone(),
            });
        }
    }
    warnings
}

#[cfg(test)]
mod tests;
