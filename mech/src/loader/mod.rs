//! End-to-end workflow loader (spec §13).
//!
//! Composes strict-field sweeps → parse → resolve schemas → validate →
//! infer function outputs → reject unsupported features
//! (`MechError::UnsupportedFeature`) → compile CEL expressions/templates into an
//! immutable, ready-to-run [`Workflow`] value. Two strict-field sweeps run
//! before the YAML parse and can surface as [`MechError::YamlParse`]:
//! [`reject_unknown_workflow_and_function_fields`] (workflow- and
//! function-scope keys, defending against `#[serde(flatten)]` silently
//! disabling `#[serde(deny_unknown_fields)]` on those parents) and
//! [`reject_unknown_block_fields`] (block-scope keys, defending against
//! the unsupported `#[serde(untagged)]` + `#[serde(flatten)]` +
//! `#[serde(deny_unknown_fields)]` combination on
//! [`crate::schema::BlockDef`] / [`crate::schema::PromptBlock`] /
//! [`crate::schema::CallBlock`] — see serde-rs/serde#1547).
//!
//! No execution logic lives here — execution lives in [`crate::exec`]. The
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
//! Load-time advisories about unimplemented features (see the
//! [`UnsupportedFeatureAdvisory`] enum for all variants, currently
//! [`UnsupportedFeatureAdvisory::CompactionUnimplemented`] and
//! [`UnsupportedFeatureAdvisory::CompactionOnDataflowFunction`]) are aggregated into a
//! hard [`MechError::UnsupportedFeature`] error and the load is rejected.
//! This trades a permissive load for a louder, earlier failure: a
//! workflow that configures e.g. `compaction:` would otherwise run
//! "successfully" and silently fail to compact at runtime (compaction
//! is not implemented — see `docs/MECH_SPEC.md` §4.6).
//! [`collect_unsupported_feature_advisories`] is retained as the internal mechanism that
//! builds the error message and remains directly testable against a
//! parsed document.
//!
//! The legacy [`WorkflowLoader`] struct is still available but new code should
//! prefer the free functions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cel::{CelExpression, Template};
use crate::error::{MechError, MechResult};
use crate::schema::{
    CALL_BLOCK_KEYS, CelSourceKind, FUNCTION_DEF_KEYS, FunctionDef, MechDocument,
    PROMPT_BLOCK_KEYS, SchemaRegistry, WORKFLOW_SECTION_KEYS, infer_function_outputs,
    parse_workflow,
};
use crate::validate::{AnyModel, ModelChecker, validate_workflow};
use crate::workflow::{Workflow, WorkflowInner};

// ---------------------------------------------------------------------------
// Free-function API
// ---------------------------------------------------------------------------

/// An advisory collected during workflow load about a configured-but-
/// unimplemented feature.
///
/// Each variant describes one offending scope. The loader collects every
/// variant produced for a document via [`collect_unsupported_feature_advisories`] and, if
/// the resulting list is non-empty, aggregates their [`Display`]
/// representations into a single
/// [`MechError::UnsupportedFeature`](crate::error::MechError::UnsupportedFeature)
/// and rejects the load — see the module-level docstring for rationale.
/// Tests construct documents and call [`collect_unsupported_feature_advisories`] directly
/// to pin which advisories fire for which configurations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedFeatureAdvisory {
    /// Workflow- or function-level `compaction:` is configured but
    /// compaction is not implemented; the loader rejects the document at
    /// load time so the workflow does not run "successfully" while
    /// silently failing to compact. The declared scope
    /// (`"workflow-level"` or `"function-level: <name>"`) is included
    /// for diagnostics.
    CompactionUnimplemented { scope: String },

    /// A dataflow function has an effective `compaction:` config (declared
    /// on the function itself or inherited from the workflow-level
    /// default). Forward-looking advisory: even once compaction is
    /// implemented, dataflow functions will not receive it because each
    /// per-block conversation is constructed empty (§4.6 rule 3) and
    /// discarded after one prompt+response, so the configured compaction
    /// would be silently discarded. Until then, the load is rejected.
    /// The named function is the dataflow function whose compaction
    /// config would be ignored.
    CompactionOnDataflowFunction { function: String },
}

impl std::fmt::Display for UnsupportedFeatureAdvisory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompactionUnimplemented { scope } => write!(
                f,
                "{scope} `compaction` is configured but compaction is not implemented. The hook only increments a counter; messages are NOT summarized. See docs/MECH_SPEC.md §4.6."
            ),
            Self::CompactionOnDataflowFunction { function } => write!(
                f,
                "function `{function}` is dataflow (no transitions, only `depends_on` edges) but has an effective `compaction:` config; even once compaction is implemented, dataflow functions will not receive it because each block runs a fresh single-turn conversation (§4.6 rule 3) and the configured compaction would be silently discarded. Until then, the load is rejected. See docs/MECH_SPEC.md §4.6."
            ),
        }
    }
}

/// Load, parse, and validate a workflow from disk.
///
/// Pipeline: read file → strict-field sweeps (reject unknown keys at
/// `workflow:`, `functions.<name>:`, and
/// `functions.<name>.blocks.<block_name>:` scopes, surface as
/// [`MechError::YamlParse`]) → parse YAML → resolve schemas →
/// validate → infer function outputs → reject unsupported features
/// (`MechError::UnsupportedFeature`) → compile CEL expressions and
/// templates.
///
/// A workflow that configures any unimplemented feature (currently
/// `compaction:` — see the module-level docstring) is rejected with
/// [`MechError::UnsupportedFeature`].
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
    load_impl(&source, Some(path.to_path_buf()), models)
}

/// Load a workflow from a YAML string.
pub fn load_workflow_str(yaml: &str) -> MechResult<Workflow> {
    load_impl(yaml, None, &AnyModel)
}

/// Load a workflow from a YAML string with a custom model checker.
pub fn load_workflow_str_with(yaml: &str, models: &dyn ModelChecker) -> MechResult<Workflow> {
    load_impl(yaml, None, models)
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
) -> MechResult<Workflow> {
    // 1. Reject unknown YAML keys via two strict-field sweeps.
    //
    // `reject_unknown_workflow_and_function_fields` covers
    // `workflow:` and `functions.<name>:` scope. Each parent embeds
    // `ExecutionConfig` via `#[serde(flatten)]` on its `defaults`
    // (`WorkflowSection`) or `overrides` (`FunctionDef`) field, which
    // silently disables `#[serde(deny_unknown_fields)]` on those
    // parents, so the strict-field check is performed here instead.
    //
    // `reject_unknown_block_fields` covers
    // `functions.<name>.blocks.<block_name>:` scope. Each block variant
    // (`PromptBlock` / `CallBlock`) carries
    // `#[serde(deny_unknown_fields)]` and flattens `BlockCommon`, and
    // is dispatched through `#[serde(untagged)]` on `BlockDef` — a
    // combination serde does not officially support
    // (serde-rs/serde#1547). The loader-side sweep enforces the
    // rejection as a load-time invariant that does not depend on serde
    // internals.
    //
    // Both sweeps run BEFORE serde parsing so that a typo like
    // `inputt:` produces an "unknown field" diagnostic rather than the
    // downstream "missing field `input`".
    reject_unknown_workflow_and_function_fields(yaml, source_path.as_ref())?;
    reject_unknown_block_fields(yaml, source_path.as_ref())?;

    // 2. Parse YAML.
    let mut file = parse_workflow(yaml).map_err(|e| MechError::YamlParse {
        path: source_path.clone(),
        message: e.to_string(),
    })?;

    // 3. Build the workflow-level shared schema registry (resolves top-level
    //    $ref-only documents, compiles every shared schema).
    let empty_schemas = BTreeMap::new();
    let schemas_map = file
        .workflow
        .as_ref()
        .map(|w| &w.schemas)
        .unwrap_or(&empty_schemas);
    let registry = SchemaRegistry::build(schemas_map)?;

    // 4. Run the §10.1 load-time validation pass. Errors → `MechError::WorkflowValidation`.
    let report = validate_workflow(&file, source_path.as_deref(), models);
    if !report.is_ok() {
        return Err(MechError::WorkflowValidation {
            errors: report.errors.iter().map(|i| i.to_string()).collect(),
        });
    }

    // Ordering invariant:
    //
    // Validation (step 4) runs BEFORE inference (step 5). Therefore any
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

    // 5. Infer function output schemas (`output: infer` / omitted).
    infer_function_outputs(&mut file)?;

    // Reject configured-but-unimplemented features (currently only
    // `compaction:`) at load time so callers cannot configure them and
    // then silently get no behavior — the runtime hook is a placeholder
    // no-op (see Conversation::check_compaction and docs/MECH_SPEC.md
    // §4.6). Aggregation mirrors MechError::WorkflowValidation: every
    // offending scope discovered by collect_unsupported_feature_advisories
    // is joined into one error message so callers see the full set in a
    // single failure.
    let advisories = collect_unsupported_feature_advisories(&file);
    if !advisories.is_empty() {
        let advisories: Vec<String> = advisories.iter().map(|a| a.to_string()).collect();
        return Err(MechError::UnsupportedFeature { advisories });
    }

    // 6. Compile every CEL guard and template in the workflow.
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
    Ok(workflow)
}

fn compile_all(
    file: &MechDocument,
    cel_expressions: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    if let Some(system) = file
        .workflow
        .as_ref()
        .and_then(|w| w.defaults.system.as_ref())
    {
        intern_template(system, templates)?;
    }
    for func in file.functions.values() {
        if let Some(system) = &func.overrides.system {
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

/// Collect every [`UnsupportedFeatureAdvisory`] advisory the document would produce.
///
/// This is the internal mechanism `load_impl` uses to build the
/// [`MechError::UnsupportedFeature`] error message — it is `pub` so tests
/// (and any out-of-tree caller that wants to inspect advisories without
/// invoking the full load pipeline) can pin which advisories fire for a
/// given parsed document. A non-empty return value means the loader will
/// reject the document; an empty return value means the unimplemented-
/// feature gate passes (other load-time errors may still apply).
pub fn collect_unsupported_feature_advisories(
    file: &MechDocument,
) -> Vec<UnsupportedFeatureAdvisory> {
    let mut advisories = Vec::new();
    if let Some(workflow) = &file.workflow {
        if workflow.defaults.compaction.is_some() {
            advisories.push(UnsupportedFeatureAdvisory::CompactionUnimplemented {
                scope: "workflow-level".to_string(),
            });
        }
    }
    for (name, func) in &file.functions {
        if func.overrides.compaction.is_some() {
            advisories.push(UnsupportedFeatureAdvisory::CompactionUnimplemented {
                scope: format!("function-level `{name}`"),
            });
        }

        // CompactionOnDataflowFunction is orthogonal to
        // CompactionUnimplemented because they describe different
        // forward-looking facts about the same configured feature:
        // CompactionUnimplemented says the runtime strategy is a global
        // no-op today; CompactionOnDataflowFunction says that even once
        // compaction is fully implemented, the config will still be
        // silently discarded on dataflow functions because dataflow
        // blocks construct a fresh single-turn `Conversation::new(None)`
        // per block (§4.6 rule 3) and never see the function-level
        // conversation. Both advisories can fire for the same function.
        // Inheritance via workflow-level default is mirrored here as a
        // pure schema check (function-level overrides, workflow-level
        // fallback) so the loader does not depend on the exec or
        // conversation layers — a dataflow function with no
        // function-level compaction but a workflow-level default still
        // produces this advisory, because that default would also be
        // silently dropped at runtime.
        let has_effective_compaction = func
            .overrides
            .resolved_compaction(file.workflow.as_ref().map(|w| &w.defaults))
            .is_some();
        if has_effective_compaction
            && crate::schema::infer_mode(func) == crate::schema::InferMode::Dataflow
        {
            advisories.push(UnsupportedFeatureAdvisory::CompactionOnDataflowFunction {
                function: name.clone(),
            });
        }
    }
    advisories
}

/// Reject unknown YAML keys at `workflow:` and `functions.<name>:` scope.
///
/// Runs in step 1 of `load_impl`, before the typed serde deserialization in
/// step 2 (`parse_workflow`). This loader-side check exists because each
/// parent embeds [`crate::schema::ExecutionConfig`] via `#[serde(flatten)]`
/// on its `defaults` ([`crate::schema::WorkflowSection`]) or `overrides`
/// ([`crate::schema::FunctionDef`]) field, which silently disables
/// `#[serde(deny_unknown_fields)]` on those parents.
/// Without this check, typos like `systm:` or `compactoin:` would parse
/// successfully with the field defaulted to None/empty.
///
/// The allow-lists [`WORKFLOW_SECTION_KEYS`] and [`FUNCTION_DEF_KEYS`] are
/// kept adjacent to the struct definitions so they stay in sync as schemas
/// evolve.
///
/// Absent or empty `workflow:` / `functions:` sections produce no errors
/// here. Non-mapping values for `workflow:` or `functions.<name>:` are
/// passed through unchecked and caught by the subsequent `parse_workflow`
/// step as serde type-mismatch errors.
pub(crate) fn reject_unknown_workflow_and_function_fields(
    yaml: &str,
    source_path: Option<&PathBuf>,
) -> MechResult<()> {
    use serde_yml::Value;

    let root: Value = serde_yml::from_str(yaml).map_err(|e| MechError::YamlParse {
        path: source_path.cloned(),
        message: e.to_string(),
    })?;
    let Some(root_map) = root.as_mapping() else {
        return Ok(());
    };

    // workflow: scope
    if let Some(workflow_val) = root_map.get(Value::String("workflow".into())) {
        if let Some(workflow_map) = workflow_val.as_mapping() {
            for k in workflow_map.keys() {
                let Some(key) = k.as_str() else { continue };
                if !WORKFLOW_SECTION_KEYS.contains(&key) {
                    return Err(MechError::YamlParse {
                        path: source_path.cloned(),
                        message: format!(
                            "unknown field `{key}` at `workflow`, expected one of {WORKFLOW_SECTION_KEYS:?}"
                        ),
                    });
                }
            }
        }
    }

    // functions.<name>: scope
    if let Some(functions_val) = root_map.get(Value::String("functions".into())) {
        if let Some(functions_map) = functions_val.as_mapping() {
            for (fn_name_val, fn_def_val) in functions_map {
                let Some(fn_def_map) = fn_def_val.as_mapping() else {
                    continue;
                };
                for k in fn_def_map.keys() {
                    let Some(key) = k.as_str() else { continue };
                    if !FUNCTION_DEF_KEYS.contains(&key) {
                        let fn_name = fn_name_val.as_str().unwrap_or("<?>");
                        return Err(MechError::YamlParse {
                            path: source_path.cloned(),
                            message: format!(
                                "unknown field `{key}` at `functions.{fn_name}`, expected one of {FUNCTION_DEF_KEYS:?}"
                            ),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

/// Reject unknown YAML keys at `functions.<name>.blocks.<block_name>:` scope.
///
/// Sibling of [`reject_unknown_workflow_and_function_fields`]; runs in
/// step 1 of `load_impl`, before serde deserialization. The
/// [`crate::schema::PromptBlock`] / [`crate::schema::CallBlock`] structs
/// each carry `#[serde(deny_unknown_fields)]` which today (in
/// combination with the `#[serde(untagged)]` [`crate::schema::BlockDef`]
/// dispatch) does reject unknown block-level keys, but that interaction
/// is officially unsupported by serde when combined with
/// `#[serde(flatten)]` (serde-rs/serde#1547). This sweep enforces the
/// same rejection as a load-time invariant that does not depend on serde
/// internals: every key under `functions.<name>.blocks.<block_name>:`
/// must appear in [`PROMPT_BLOCK_KEYS`] or [`CALL_BLOCK_KEYS`],
/// selected by the discriminator key (`prompt:` vs `call:`).
///
/// Blocks with neither (or both) discriminator keys are passed through
/// unchecked here; they are diagnosed by serde's untagged-enum
/// dispatch on `BlockDef` during the subsequent `parse_workflow` step,
/// which surfaces as [`MechError::YamlParse`].
///
/// Non-mapping values for any of the surrounding scopes are passed
/// through unchecked and caught by the subsequent `parse_workflow` step
/// as serde type-mismatch errors.
pub(crate) fn reject_unknown_block_fields(
    yaml: &str,
    source_path: Option<&PathBuf>,
) -> MechResult<()> {
    use serde_yml::Value;

    let root: Value = serde_yml::from_str(yaml).map_err(|e| MechError::YamlParse {
        path: source_path.cloned(),
        message: e.to_string(),
    })?;
    let Some(root_map) = root.as_mapping() else {
        return Ok(());
    };

    let Some(functions_val) = root_map.get(Value::String("functions".into())) else {
        return Ok(());
    };
    let Some(functions_map) = functions_val.as_mapping() else {
        return Ok(());
    };

    for (fn_name_val, fn_def_val) in functions_map {
        let Some(fn_def_map) = fn_def_val.as_mapping() else {
            continue;
        };
        let Some(blocks_val) = fn_def_map.get(Value::String("blocks".into())) else {
            continue;
        };
        let Some(blocks_map) = blocks_val.as_mapping() else {
            continue;
        };

        for (block_name_val, block_def_val) in blocks_map {
            let Some(block_def_map) = block_def_val.as_mapping() else {
                continue;
            };

            let has_prompt = block_def_map.contains_key(Value::String("prompt".into()));
            let has_call = block_def_map.contains_key(Value::String("call".into()));
            // Ambiguous / missing-discriminator blocks are diagnosed by
            // serde's untagged-enum dispatch on `BlockDef` during the
            // subsequent `parse_workflow` step; do not double-report here.
            let allow_list: &[&str] = match (has_prompt, has_call) {
                (true, false) => PROMPT_BLOCK_KEYS,
                (false, true) => CALL_BLOCK_KEYS,
                _ => continue,
            };

            for k in block_def_map.keys() {
                let Some(key) = k.as_str() else { continue };
                if !allow_list.contains(&key) {
                    let fn_name = fn_name_val.as_str().unwrap_or("<?>");
                    let block_name = block_name_val.as_str().unwrap_or("<?>");
                    return Err(MechError::YamlParse {
                        path: source_path.cloned(),
                        message: format!(
                            "unknown field `{key}` at `functions.{fn_name}.blocks.{block_name}`, expected one of {allow_list:?}"
                        ),
                    });
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
