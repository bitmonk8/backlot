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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cel::{CelExpression, Template};
use crate::error::{MechError, MechResult};
use crate::schema::{
    BlockDef, CallBlock, CallSpec, FunctionDef, PromptBlock, SchemaRegistry, WorkflowFile,
    infer_function_outputs, parse_workflow,
};
use crate::validate::{AnyModel, ModelChecker, validate_workflow};

/// An immutable, fully-validated workflow, ready for execution.
///
/// Produced by [`WorkflowLoader::load`] (or [`WorkflowLoader::load_str`] for
/// in-memory tests). Holds:
///
/// * The parsed and inference-resolved [`WorkflowFile`].
/// * A compiled [`SchemaRegistry`] covering every workflow-level shared
///   schema.
/// * A deduplicated cache of compiled [`CelExpression`] guards (keyed by
///   source text) — every `when:` clause in the workflow.
/// * A deduplicated cache of compiled [`Template`] strings (keyed by source
///   text) — every `prompt:`, `set_context` / `set_workflow` value, and every
///   `input` / `output` mapping value on a call block.
///
/// The struct is `Send + Sync` and deliberately uses [`BTreeMap`] so that
/// iteration order — and therefore any debug / error output derived from it —
/// is deterministic.
#[derive(Debug, Clone)]
pub struct Workflow {
    file: Arc<WorkflowFile>,
    source_path: Option<PathBuf>,
    schemas: Arc<SchemaRegistry>,
    guards: Arc<BTreeMap<String, Arc<CelExpression>>>,
    templates: Arc<BTreeMap<String, Arc<Template>>>,
}

impl Workflow {
    /// The parsed, validated, inference-resolved workflow file.
    pub fn file(&self) -> &WorkflowFile {
        &self.file
    }

    /// The path the workflow was loaded from, if any.
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// The compiled shared-schema registry.
    pub fn schemas(&self) -> &SchemaRegistry {
        &self.schemas
    }

    /// Look up a compiled guard expression by source text.
    pub fn guard(&self, source: &str) -> Option<&Arc<CelExpression>> {
        self.guards.get(source)
    }

    /// Look up a compiled template by source text.
    pub fn template(&self, source: &str) -> Option<&Arc<Template>> {
        self.templates.get(source)
    }

    /// Number of distinct compiled guard expressions.
    pub fn guard_count(&self) -> usize {
        self.guards.len()
    }

    /// Number of distinct compiled templates.
    pub fn template_count(&self) -> usize {
        self.templates.len()
    }
}

/// Configurable workflow loader.
///
/// The default configuration accepts any agent model name (via [`AnyModel`]).
/// Tests or CLIs that want stricter model resolution can swap in a different
/// [`ModelChecker`] via [`WorkflowLoader::with_model_checker`].
pub struct WorkflowLoader {
    models: Box<dyn ModelChecker>,
}

impl Default for WorkflowLoader {
    fn default() -> Self {
        Self {
            models: Box::new(AnyModel),
        }
    }
}

impl std::fmt::Debug for WorkflowLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowLoader").finish_non_exhaustive()
    }
}

impl WorkflowLoader {
    /// Create a loader with the default (`AnyModel`) model checker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the model checker used during the validate pass.
    pub fn with_model_checker<M: ModelChecker + 'static>(mut self, models: M) -> Self {
        self.models = Box::new(models);
        self
    }

    /// Load, parse, and validate a workflow from disk.
    ///
    /// Pipeline: read file → parse YAML → build schema registry → validate →
    /// infer function outputs → compile CEL guards and templates.
    pub fn load(&self, path: impl AsRef<Path>) -> MechResult<Workflow> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|e| MechError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        self.load_impl(&source, Some(path.to_path_buf()))
    }

    /// Load a workflow directly from a YAML string. Used by tests and
    /// embedded callers with no on-disk file.
    pub fn load_str(&self, yaml: &str) -> MechResult<Workflow> {
        self.load_impl(yaml, None)
    }

    fn load_impl(&self, yaml: &str, source_path: Option<PathBuf>) -> MechResult<Workflow> {
        // 1. Parse YAML.
        let mut file = parse_workflow(yaml).map_err(|e| MechError::YamlParse {
            path: source_path.clone().unwrap_or_default(),
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

        // 3. Run the §10.1 load-time validation pass. Errors → `MechError::Validation`.
        let report = validate_workflow(&file, source_path.as_deref(), self.models.as_ref());
        if !report.is_ok() {
            return Err(MechError::Validation {
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

        // 5. Compile every CEL guard and template in the workflow.
        let mut guards: BTreeMap<String, Arc<CelExpression>> = BTreeMap::new();
        let mut templates: BTreeMap<String, Arc<Template>> = BTreeMap::new();
        compile_all(&file, &mut guards, &mut templates)?;

        Ok(Workflow {
            file: Arc::new(file),
            source_path,
            schemas: Arc::new(registry),
            guards: Arc::new(guards),
            templates: Arc::new(templates),
        })
    }
}

fn compile_all(
    file: &WorkflowFile,
    guards: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    // Workflow-level `system` is a template string (spec §7) and can contain
    // `{{...}}` interpolation. Compile it so the "every template compiled at
    // load" invariant holds.
    if let Some(system) = file.workflow.as_ref().and_then(|w| w.system.as_ref()) {
        intern_template(system, templates)?;
    }
    for func in file.functions.values() {
        // Function-level `system` override is also a template string (§7).
        if let Some(system) = &func.system {
            intern_template(system, templates)?;
        }
        compile_function(func, guards, templates)?;
    }
    Ok(())
}

fn compile_function(
    func: &FunctionDef,
    guards: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    for block in func.blocks.values() {
        match block {
            BlockDef::Prompt(p) => compile_prompt(p, guards, templates)?,
            BlockDef::Call(c) => compile_call(c, guards, templates)?,
        }
    }
    Ok(())
}

fn compile_prompt(
    p: &PromptBlock,
    guards: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    intern_template(&p.prompt, templates)?;
    for expr in p.set_context.values() {
        intern_guard(expr, guards)?;
    }
    for expr in p.set_workflow.values() {
        intern_guard(expr, guards)?;
    }
    for t in &p.transitions {
        if let Some(w) = &t.when {
            intern_guard(w, guards)?;
        }
    }
    Ok(())
}

fn compile_call(
    c: &CallBlock,
    guards: &mut BTreeMap<String, Arc<CelExpression>>,
    templates: &mut BTreeMap<String, Arc<Template>>,
) -> MechResult<()> {
    if let Some(input) = &c.input {
        for expr in input.values() {
            intern_template(expr, templates)?;
        }
    }
    if let CallSpec::PerCall(entries) = &c.call {
        for entry in entries {
            for expr in entry.input.values() {
                intern_template(expr, templates)?;
            }
        }
    }
    if let Some(output) = &c.output {
        for expr in output.values() {
            intern_template(expr, templates)?;
        }
    }
    for expr in c.set_context.values() {
        intern_guard(expr, guards)?;
    }
    for expr in c.set_workflow.values() {
        intern_guard(expr, guards)?;
    }
    for t in &c.transitions {
        if let Some(w) = &t.when {
            intern_guard(w, guards)?;
        }
    }
    Ok(())
}

fn intern_guard(source: &str, guards: &mut BTreeMap<String, Arc<CelExpression>>) -> MechResult<()> {
    // Note: `validate.rs` has already compiled every guard via
    // `CelExpression::compile` and would have surfaced any compile error as
    // `MechError::Validation`. By the time we get here, compilation cannot
    // fail — but we still recompile to populate the interning cache.
    if guards.contains_key(source) {
        return Ok(());
    }
    let compiled = CelExpression::compile(source)?;
    guards.insert(source.to_string(), Arc::new(compiled));
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

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_EXAMPLE: &str = include_str!("schema/full_example.yaml");

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn workflow_is_send_sync() {
        assert_send_sync::<Workflow>();
    }

    #[test]
    fn loads_full_worked_example() {
        let loader = WorkflowLoader::new();
        let wf = loader
            .load_str(FULL_EXAMPLE)
            .expect("full §12 example must load");

        // Function count: support_triage + resolve_billing.
        assert_eq!(wf.file().functions.len(), 2, "two functions expected");
        let triage = wf
            .file()
            .functions
            .get("support_triage")
            .expect("support_triage present");
        // Block count from the §12 example: classify, billing, technical,
        // general, escalate, respond = 6.
        assert_eq!(triage.blocks.len(), 6, "six blocks in support_triage");

        // Agents from the workflow-level defaults.
        let agents = &wf.file().workflow.as_ref().unwrap().agents;
        assert!(agents.contains_key("default"));
        assert!(agents.contains_key("diagnostician"));

        // At least one guard was compiled (the classify transitions have
        // `when:` clauses) and several templates (all prompts and call input
        // mappings).
        assert!(wf.guard_count() > 0, "expected compiled guards");
        assert!(wf.template_count() > 0, "expected compiled templates");

        // Spot-check a specific guard source is interned.
        assert!(
            wf.guard("output.category == \"billing\"").is_some(),
            "classify -> billing guard should be compiled and interned"
        );
    }

    #[test]
    fn load_is_deterministic() {
        // Pin determinism against the specific §12 fixture: sorted guard keys
        // must exactly match a known-expected list, and the template key set
        // must contain specific known entries. This catches drift in the
        // compile pass (e.g. missed template locations) in addition to
        // BTreeMap iteration stability.
        let loader = WorkflowLoader::new();
        let wf = loader.load_str(FULL_EXAMPLE).unwrap();

        let guards: Vec<&str> = wf.guards.keys().map(String::as_str).collect();
        let expected_guards = vec![
            "context.attempts + 1",
            "context.attempts < 3",
            "output.category == \"billing\"",
            "output.category == \"technical\"",
            "size(output.steps) > 0",
        ];
        assert_eq!(
            guards, expected_guards,
            "guard key set must exactly match the §12 fixture"
        );

        // Template keys: must include workflow-level `system` (finding 1),
        // function-level `system` override, and each block prompt.
        let tmpl_keys: Vec<&str> = wf.templates.keys().map(String::as_str).collect();
        assert!(
            tmpl_keys.contains(&"You are a customer support agent."),
            "workflow-level `system` template must be interned; got {tmpl_keys:?}"
        );
        assert!(
            tmpl_keys
                .contains(&"You are a billing specialist. Be precise about amounts and dates."),
            "function-level `system` override must be interned; got {tmpl_keys:?}"
        );
        assert!(
            tmpl_keys
                .iter()
                .any(|k| k.contains("Classify the following")),
            "classify prompt must be interned"
        );

        // And a second load must produce the same key sets.
        let wf2 = loader.load_str(FULL_EXAMPLE).unwrap();
        let guards2: Vec<&str> = wf2.guards.keys().map(String::as_str).collect();
        let tmpl_keys2: Vec<&str> = wf2.templates.keys().map(String::as_str).collect();
        assert_eq!(guards, guards2, "guard iteration must be stable");
        assert_eq!(tmpl_keys, tmpl_keys2, "template iteration must be stable");
    }

    #[test]
    fn missing_file_yields_io_error() {
        let loader = WorkflowLoader::new();
        let err = loader
            .load("/definitely/not/a/real/mech/workflow.yaml")
            .expect_err("missing file must error");
        assert!(
            matches!(err, MechError::Io { .. }),
            "expected MechError::Io, got {err:?}"
        );
    }

    #[test]
    fn bad_yaml_yields_yaml_parse_error() {
        let loader = WorkflowLoader::new();
        let err = loader
            .load_str("functions: [this is : not valid")
            .expect_err("malformed YAML must error");
        assert!(
            matches!(err, MechError::YamlParse { .. }),
            "expected MechError::YamlParse, got {err:?}"
        );
    }

    #[test]
    fn semantic_error_yields_validation_variant() {
        // A transition references an undefined block — caught by §10.1.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object }
        transitions:
          - goto: nowhere
"#;
        let loader = WorkflowLoader::new();
        let err = loader
            .load_str(yaml)
            .expect_err("semantic error must error");
        match err {
            MechError::Validation { errors } => {
                assert!(
                    !errors.is_empty(),
                    "validation error list must be non-empty"
                );
            }
            other => panic!("expected MechError::Validation, got {other:?}"),
        }
    }

    #[test]
    fn load_from_disk_roundtrips_via_tempfile() {
        use std::io::Write;
        // Write the §12 example to a temp file and load it through `load`.
        let mut dir = std::env::temp_dir();
        dir.push(format!("mech-loader-test-{}.yaml", std::process::id()));
        {
            let mut f = std::fs::File::create(&dir).expect("create temp file");
            f.write_all(FULL_EXAMPLE.as_bytes()).expect("write yaml");
        }
        let loader = WorkflowLoader::new();
        let wf = loader.load(&dir).expect("load from disk must succeed");
        assert_eq!(wf.source_path(), Some(dir.as_path()));
        assert_eq!(wf.file().functions.len(), 2);
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn cel_compile_error_surfaces() {
        // validate.rs compiles every guard via `CelExpression::compile` and
        // aggregates failures into `MechError::Validation`. The loader's
        // compile pass never sees a bad-CEL guard — by the time it runs,
        // validation has already failed. So the deterministic variant here
        // is `Validation`, not `CelCompilation`.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object }
        transitions:
          - when: "output.x +"
            goto: b
      b:
        prompt: "bye"
        schema: { type: object }
"#;
        let loader = WorkflowLoader::new();
        let err = loader.load_str(yaml).expect_err("bad CEL must error");
        match err {
            MechError::Validation { errors } => {
                assert!(
                    errors.iter().any(|e| e.contains("CEL compile error")),
                    "expected a CEL compile error message, got {errors:?}"
                );
            }
            other => panic!("expected MechError::Validation, got {other:?}"),
        }
    }

    #[test]
    fn inference_failure_surfaces() {
        // A function with `output: infer` and a cycle (no terminals) cannot
        // produce an output schema. Inference must surface
        // `MechError::InferenceFailed`.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      classify:
        prompt: "classify"
        schema:
          type: object
          required: [x]
          properties:
            x: { type: string }
        transitions:
          - when: "true"
            goto: a
          - goto: b
      a:
        prompt: "a"
        schema:
          type: object
          required: [x]
          properties:
            x: { type: string }
        depends_on: [classify]
      b:
        prompt: "b"
        schema:
          type: object
          required: [y]
          properties:
            y: { type: integer }
        depends_on: [classify]
"#;
        let loader = WorkflowLoader::new();
        let err = loader
            .load_str(yaml)
            .expect_err("incompatible terminal outputs must error");
        assert!(
            matches!(err, MechError::InferenceFailed { .. }),
            "expected MechError::InferenceFailed, got {err:?}"
        );
    }

    #[test]
    fn bad_template_surfaces() {
        // A malformed `{{...}}` (unterminated) in a prompt block is caught by
        // validate.rs's template extraction pass and surfaces as
        // `MechError::Validation`. Pin to that deterministic variant.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hello {{ input.name"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        let loader = WorkflowLoader::new();
        let err = loader
            .load_str(yaml)
            .expect_err("malformed template must error");
        assert!(
            matches!(err, MechError::Validation { .. }),
            "expected MechError::Validation, got {err:?}"
        );
    }

    #[test]
    fn validation_runs_before_inference() {
        // Contract: validation runs before inference, and a function declaring
        // `output: infer` must still pass validation and end up with a concrete
        // inferred output schema matching its terminal block's schema.
        use crate::schema::SchemaRef;
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        let loader = WorkflowLoader::new();
        let wf = loader
            .load_str(yaml)
            .expect("validation must succeed and inference must resolve output");
        let func = wf.file().functions.get("f").expect("function f present");
        let inline = match &func.output {
            Some(SchemaRef::Inline(v)) => v,
            other => panic!("expected inferred inline output schema, got {other:?}"),
        };
        // Pin the inferred schema's shape to block `a`'s schema — guards
        // against regressions where inference leaves a default/empty schema.
        assert_eq!(inline.get("type").and_then(|v| v.as_str()), Some("object"));
        let required: Vec<&str> = inline
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(required, vec!["answer"]);
        assert_eq!(
            inline
                .get("properties")
                .and_then(|p| p.get("answer"))
                .and_then(|a| a.get("type"))
                .and_then(|v| v.as_str()),
            Some("string"),
        );
    }

    #[test]
    fn invalid_infer_output_function_fails_at_validation_not_inference() {
        // Ordering guard: a function that declares `output: infer` AND has an
        // independent validation error (undefined transition target) must
        // surface the validation error — not an inference error and not
        // success. Pins that validation is not skipped for infer-output
        // functions.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
        transitions:
          - goto: nowhere
"#;
        let loader = WorkflowLoader::new();
        let err = loader
            .load_str(yaml)
            .expect_err("undefined transition target must fail validation");
        match err {
            MechError::Validation { errors } => {
                assert!(
                    !errors.is_empty(),
                    "validation error list must be non-empty"
                );
            }
            other => panic!("expected MechError::Validation, got {other:?}"),
        }
    }

    #[test]
    fn workflow_level_system_template_is_compiled() {
        // Finding 1: `workflow.system` is a template string and must be
        // compiled at load time and retrievable via `Workflow::template`.
        let yaml = r#"
workflow:
  system: "You are helping {{input.user}}."
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        let loader = WorkflowLoader::new();
        let wf = loader.load_str(yaml).expect("must load");
        assert!(
            wf.template("You are helping {{input.user}}.").is_some(),
            "workflow-level `system` template must be compiled and interned; \
             have keys: {:?}",
            wf.templates.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn example_imperative_routing_loads() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/imperative_routing.yaml");
        let result = WorkflowLoader::new().load(&path);
        assert!(
            result.is_ok(),
            "imperative_routing.yaml failed to load: {:?}",
            result.err()
        );
    }

    #[test]
    fn example_dataflow_pipeline_loads() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/dataflow_pipeline.yaml");
        let result = WorkflowLoader::new().load(&path);
        assert!(
            result.is_ok(),
            "dataflow_pipeline.yaml failed to load: {:?}",
            result.err()
        );
    }

    #[test]
    fn example_function_composition_loads() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/function_composition.yaml");
        let result = WorkflowLoader::new().load(&path);
        assert!(
            result.is_ok(),
            "function_composition.yaml failed to load: {:?}",
            result.err()
        );
    }
}
