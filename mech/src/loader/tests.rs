use super::*;
use crate::loader::{LoadWarning, collect_load_warnings};
use crate::schema::parse_workflow;

const FULL_EXAMPLE: &str = include_str!("../../testdata/full_example.yaml");

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn workflow_is_send_sync() {
    assert_send_sync::<Workflow>();
}

#[test]
fn loads_full_worked_example() {
    let wf = load_workflow_str(FULL_EXAMPLE).expect("full §12 example must load");

    // Function count: support_triage + resolve_billing.
    assert_eq!(wf.document().functions.len(), 2, "two functions expected");
    let triage = wf
        .document()
        .functions
        .get("support_triage")
        .expect("support_triage present");
    // Block count from the §12 example: classify, billing, technical,
    // general, escalate, respond = 6.
    assert_eq!(triage.blocks.len(), 6, "six blocks in support_triage");

    // Agents from the workflow-level defaults.
    let agents = &wf.document().workflow.as_ref().unwrap().agents;
    assert!(agents.contains_key("default"));
    assert!(agents.contains_key("diagnostician"));

    // At least one guard was compiled (the classify transitions have
    // `when:` clauses) and several templates (all prompts and call input
    // mappings).
    assert!(
        wf.cel_expression_count() > 0,
        "expected compiled CEL expressions"
    );
    assert!(wf.template_count() > 0, "expected compiled templates");

    // Spot-check a specific guard source is interned.
    assert!(
        wf.cel_expression("output.category == \"billing\"")
            .is_some(),
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
    let wf = load_workflow_str(FULL_EXAMPLE).unwrap();

    let cel_exprs: Vec<&str> = wf.0.cel_expressions.keys().map(String::as_str).collect();
    let expected_cel_exprs = vec![
        "context.attempts + 1",
        "context.attempts < 3",
        "output.category == \"billing\"",
        "output.category == \"technical\"",
        "size(output.steps) > 0",
    ];
    assert_eq!(
        cel_exprs, expected_cel_exprs,
        "CEL expression key set must exactly match the §12 fixture"
    );

    // Template keys: must include workflow-level `system` (finding 1),
    // function-level `system` override, and each block prompt.
    let tmpl_keys: Vec<&str> = wf.0.templates.keys().map(String::as_str).collect();
    assert!(
        tmpl_keys.contains(&"You are a customer support agent."),
        "workflow-level `system` template must be interned; got {tmpl_keys:?}"
    );
    assert!(
        tmpl_keys.contains(&"You are a billing specialist. Be precise about amounts and dates."),
        "function-level `system` override must be interned; got {tmpl_keys:?}"
    );
    assert!(
        tmpl_keys
            .iter()
            .any(|k| k.contains("Classify the following")),
        "classify prompt must be interned"
    );

    // And a second load must produce the same key sets.
    let wf2 = load_workflow_str(FULL_EXAMPLE).unwrap();
    let cel_exprs2: Vec<&str> = wf2.0.cel_expressions.keys().map(String::as_str).collect();
    let tmpl_keys2: Vec<&str> = wf2.0.templates.keys().map(String::as_str).collect();
    assert_eq!(
        cel_exprs, cel_exprs2,
        "CEL expression iteration must be stable"
    );
    assert_eq!(tmpl_keys, tmpl_keys2, "template iteration must be stable");
}

#[test]
fn missing_file_yields_io_error() {
    let err = load_workflow("/definitely/not/a/real/mech/workflow.yaml")
        .expect_err("missing file must error");
    assert!(
        matches!(err, MechError::Io { .. }),
        "expected MechError::Io, got {err:?}"
    );
}

#[test]
fn bad_yaml_yields_yaml_parse_error() {
    let err = load_workflow_str("functions: [this is : not valid")
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
    let err = load_workflow_str(yaml).expect_err("semantic error must error");
    match err {
        MechError::WorkflowValidation { errors } => {
            assert!(
                !errors.is_empty(),
                "validation error list must be non-empty"
            );
        }
        other => panic!("expected MechError::WorkflowValidation, got {other:?}"),
    }
}

// System temp (`%TEMP%`, under `C:\Users\...`) requires elevation for
// AppContainer traverse ACE grants. Use a project-local gitignored directory
// (`<workspace>/test_tmp/`) instead so this test stays passing once mech ever
// loads workflows from sandbox-granted paths. Pattern matches lot/reel/epic.
fn ensure_project_test_tmp_dir() -> std::path::PathBuf {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_tmp");
    std::fs::create_dir_all(&base).expect("create workspace test_tmp dir");
    base
}

#[test]
fn load_from_disk_roundtrips_via_tempfile() {
    let dir = tempfile::TempDir::new_in(ensure_project_test_tmp_dir()).expect("create temp dir");
    let path = dir.path().join("workflow.yaml");
    std::fs::write(&path, FULL_EXAMPLE.as_bytes()).expect("write yaml");
    let wf = load_workflow(&path).expect("load from disk must succeed");
    assert_eq!(wf.source_path(), Some(path.as_path()));
    assert_eq!(wf.document().functions.len(), 2);
}

#[test]
fn cel_compile_error_surfaces() {
    // validate.rs compiles every guard via `CelExpression::compile` and
    // aggregates failures into `MechError::WorkflowValidation`. The loader's
    // compile pass never sees a bad-CEL guard — by the time it runs,
    // validation has already failed. So the deterministic variant here
    // is `WorkflowValidation`, not `CelCompilation`.
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
    let err = load_workflow_str(yaml).expect_err("bad CEL must error");
    match err {
        MechError::WorkflowValidation { errors } => {
            assert!(
                errors.iter().any(|e| e.contains("CEL compile error")),
                "expected a CEL compile error message, got {errors:?}"
            );
        }
        other => panic!("expected MechError::WorkflowValidation, got {other:?}"),
    }
}

#[test]
fn inference_failure_surfaces() {
    // A function with `output: infer` and a cycle (no terminals) cannot
    // produce an output schema. Inference must surface
    // `MechError::OutputSchemaInferenceFailed`.
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
    let err = load_workflow_str(yaml).expect_err("incompatible terminal outputs must error");
    assert!(
        matches!(err, MechError::OutputSchemaInferenceFailed { .. }),
        "expected MechError::OutputSchemaInferenceFailed, got {err:?}"
    );
}

#[test]
fn bad_template_surfaces() {
    // A malformed `{{...}}` (unterminated) in a prompt block is caught by
    // validate.rs's template extraction pass and surfaces as
    // `MechError::WorkflowValidation`. Pin to that deterministic variant.
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
    let err = load_workflow_str(yaml).expect_err("malformed template must error");
    assert!(
        matches!(err, MechError::WorkflowValidation { .. }),
        "expected MechError::WorkflowValidation, got {err:?}"
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
    let wf =
        load_workflow_str(yaml).expect("validation must succeed and inference must resolve output");
    let func = wf
        .document()
        .functions
        .get("f")
        .expect("function f present");
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
    let err =
        load_workflow_str(yaml).expect_err("undefined transition target must fail validation");
    match err {
        MechError::WorkflowValidation { errors } => {
            assert!(
                !errors.is_empty(),
                "validation error list must be non-empty"
            );
        }
        other => panic!("expected MechError::WorkflowValidation, got {other:?}"),
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
    let wf = load_workflow_str(yaml).expect("must load");
    assert!(
        wf.template("You are helping {{input.user}}.").is_some(),
        "workflow-level `system` template must be compiled and interned; \
         have keys: {:?}",
        wf.0.templates.keys().collect::<Vec<_>>()
    );
}

#[test]
fn example_imperative_routing_loads() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/imperative_routing.yaml");
    let result = load_workflow(&path);
    assert!(
        result.is_ok(),
        "imperative_routing.yaml failed to load: {:?}",
        result.err()
    );
}

#[test]
fn example_dataflow_pipeline_loads() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/dataflow_pipeline.yaml");
    let result = load_workflow(&path);
    assert!(
        result.is_ok(),
        "dataflow_pipeline.yaml failed to load: {:?}",
        result.err()
    );
}

#[test]
fn example_function_composition_loads() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/function_composition.yaml");
    let result = load_workflow(&path);
    assert!(
        result.is_ok(),
        "function_composition.yaml failed to load: {:?}",
        result.err()
    );
}
// ---- Loader edge cases ----

#[test]
fn empty_functions_map_errors() {
    let yaml = "functions: {}
";
    let err = load_workflow_str(yaml).expect_err("empty functions must error");
    assert!(matches!(err, MechError::WorkflowValidation { .. }));
}

#[test]
fn omitted_workflow_block_loads() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      done:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
    let wf = load_workflow_str(yaml).expect("workflow without `workflow:` block must load");
    assert!(wf.document().workflow.is_none());
    assert_eq!(wf.document().functions.len(), 1);
}

#[test]
fn duplicate_cel_expressions_are_deduplicated() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "classify"
        schema:
          type: object
          required: [x]
          properties:
            x: { type: string }
        transitions:
          - when: "output.x == \"yes\""
            goto: b
          - when: "output.x == \"yes\""
            goto: c
          - goto: c
      b:
        prompt: "b"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      c:
        prompt: "c"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let wf = load_workflow_str(yaml).expect("must load");
    // Two identical CEL expressions should deduplicate to one entry
    assert_eq!(
        wf.cel_expression_count(),
        1,
        "duplicate CEL expressions must deduplicate to 1"
    );
    assert!(wf.cel_expression(r#"output.x == "yes""#).is_some());
    assert_eq!(
        wf.template_count(),
        3,
        "three distinct prompts must each be interned"
    );
}

#[test]
fn rejecting_model_checker_propagates_error() {
    use crate::validate::KnownModels;
    let yaml = r#"
workflow:
  agents:
    a:
      model: unknown_model
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let known = KnownModels::new(["sonnet".to_string()]);
    let err = load_workflow_str_with(yaml, &known).expect_err("unknown model must error");
    assert!(matches!(err, MechError::WorkflowValidation { .. }));
}

#[test]
fn resolve_billing_block_count() {
    let wf = load_workflow_str(FULL_EXAMPLE).expect("full example must load");
    let billing = wf
        .document()
        .functions
        .get("resolve_billing")
        .expect("resolve_billing present");
    assert_eq!(
        billing.blocks.len(),
        2,
        "resolve_billing must have 2 blocks (analyze, resolve)"
    );
}

#[test]
fn cyclic_shared_schema_errors_at_loader() {
    let yaml = r##"
workflow:
  schemas:
    a:
      $ref: "#b"
    b:
      $ref: "#a"
functions:
  f:
    input: { type: object }
    blocks:
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"##;
    let err = load_workflow_str(yaml).expect_err("cyclic schema must error");
    match err {
        MechError::SchemaRefCircular { chain } => {
            assert!(chain.contains(&"a".to_string()), "chain must include 'a'");
            assert!(chain.contains(&"b".to_string()), "chain must include 'b'");
        }
        other => panic!("expected MechError::SchemaRefCircular, got {other:?}"),
    }
}

// ---- Legacy WorkflowLoader still works ----

#[test]
fn legacy_workflow_loader_load_str() {
    let loader = WorkflowLoader::new();
    let wf = loader
        .load_str(FULL_EXAMPLE)
        .expect("must load via legacy API");
    assert_eq!(wf.document().functions.len(), 2);
}

// ---- compaction-config warning at load time -----------------------------

// Load-time advisories are exposed as a `Vec<LoadWarning>` via
// `collect_load_warnings(&parsed_document)`. Tests parse the YAML
// independently and call the collector directly so they pin the advisory
// list without needing a `tracing::Subscriber`. The loader pipes the same
// list through `tracing::warn!` for production observability.

fn count_compaction_warnings_for_scope(warnings: &[LoadWarning], scope: &str) -> usize {
    warnings
        .iter()
        .filter(|w| match w {
            LoadWarning::CompactionPlaceholder { scope: s } => s.contains(scope),
            // The placeholder-scope counter is for the global "compaction is
            // a no-op" advisory; the dataflow-discard advisory is a
            // different warning class and is counted separately by
            // `count_compaction_on_dataflow_warnings_for_function`.
            LoadWarning::CompactionOnDataflowFunction { .. } => false,
        })
        .count()
}

fn count_compaction_on_dataflow_warnings_for_function(
    warnings: &[LoadWarning],
    func: &str,
) -> usize {
    warnings
        .iter()
        .filter(|w| match w {
            LoadWarning::CompactionOnDataflowFunction { function } => function == func,
            LoadWarning::CompactionPlaceholder { .. } => false,
        })
        .count()
}

#[test]
fn workflow_level_compaction_emits_load_time_warning() {
    let yaml = r#"
workflow:
  compaction:
    keep_recent_tokens: 1000
    reserve_tokens: 1000
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
    // Round-trip the YAML through the loader to ensure it is still valid,
    // then parse + collect warnings against the parsed document.
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "workflow-level"),
        1,
        "expected exactly one workflow-level compaction warning, got: {warnings:?}"
    );
    // No function-level compaction was configured.
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level"),
        0
    );
}

#[test]
fn function_level_compaction_emits_load_time_warning() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 800
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
    // Round-trip the YAML through the loader to ensure it is still valid,
    // then parse + collect warnings against the parsed document.
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level `f`"),
        1,
        "expected exactly one function-level compaction warning, got: {warnings:?}"
    );
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "workflow-level"),
        0
    );
}

#[test]
fn workflow_without_compaction_emits_no_warning() {
    let yaml = r#"
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
    // Round-trip the YAML through the loader to ensure it is still valid,
    // then parse + collect warnings against the parsed document.
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);
    assert!(
        warnings.is_empty(),
        "expected no warnings for a workflow without compaction config, got: {warnings:?}"
    );
}

// When both workflow- and function-level `compaction:` are configured,
// both warnings must fire (one of each, neither suppressing the other).
#[test]
fn combined_workflow_and_function_compaction_emits_two_warnings() {
    let yaml = r#"
workflow:
  compaction:
    keep_recent_tokens: 1000
    reserve_tokens: 1000
functions:
  f:
    input: { type: object }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 800
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
    // Round-trip the YAML through the loader to ensure it is still valid,
    // then parse + collect warnings against the parsed document.
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);
    assert_eq!(
        warnings.len(),
        2,
        "expected exactly two compaction warnings (one workflow + one function), got: {warnings:?}"
    );
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "workflow-level"),
        1,
        "expected one workflow-level warning, got: {warnings:?}"
    );
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level `f`"),
        1,
        "expected one function-level warning, got: {warnings:?}"
    );
}

// Two function-level `compaction` configs (no workflow-level) must both
// emit warnings — pins the loader's `filter` over `find` semantics so a
// future short-circuit refactor would surface in tests rather than ship.
#[test]
fn two_function_level_compactions_emit_two_warnings() {
    let yaml = r#"
functions:
  f1:
    input: { type: object }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 800
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
  f2:
    input: { type: object }
    compaction:
      keep_recent_tokens: 600
      reserve_tokens: 900
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function"),
        2,
        "expected exactly two function-level compaction warnings, got: {warnings:?}"
    );
}

/// Build a workflow YAML for the dataflow-warning test matrix.
/// `workflow_compaction` and `function_compaction` are the body lines that go
/// under `workflow.compaction:` (4-space indent) and `functions.f.compaction:`
/// (6-space indent) respectively, or `None` to omit the section. `mode`
/// selects the block topology: `"dataflow"` (b depends_on a) or
/// `"imperative"` (a transitions to b).
fn compaction_warning_yaml(
    workflow_compaction: Option<&str>,
    function_compaction: Option<&str>,
    mode: &str,
) -> String {
    let workflow_section = match workflow_compaction {
        Some(body) => format!(
            "workflow:
  compaction:
{body}
"
        ),
        None => String::new(),
    };
    let function_compaction_section = match function_compaction {
        Some(body) => format!(
            "    compaction:
{body}
"
        ),
        None => String::new(),
    };
    let blocks = match mode {
        "dataflow" => concat!(
            "      a:
",
            "        prompt: \"root\"
",
            "        schema:
",
            "          type: object
",
            "          required: [x]
",
            "          properties: { x: { type: integer } }
",
            "      b:
",
            "        prompt: \"leaf {{block.a.output.x}}\"
",
            "        schema:
",
            "          type: object
",
            "          required: [y]
",
            "          properties: { y: { type: string } }
",
            "        depends_on: [a]",
        ),
        "imperative" => concat!(
            "      a:
",
            "        prompt: \"step a\"
",
            "        schema:
",
            "          type: object
",
            "          required: [x]
",
            "          properties: { x: { type: string } }
",
            "        transitions:
",
            "          - goto: b
",
            "      b:
",
            "        prompt: \"step b\"
",
            "        schema:
",
            "          type: object
",
            "          required: [y]
",
            "          properties: { y: { type: string } }",
        ),
        _ => panic!("compaction_warning_yaml: unknown mode {mode:?}"),
    };
    format!(
        "{workflow_section}functions:
  f:
    input: {{ type: object }}
{function_compaction_section}    blocks:
{blocks}
",
    )
}

// ---- compaction on dataflow function ------------------------------------

// Function-level `compaction:` declared on a dataflow function (no
// transitions, only `depends_on` edges) emits BOTH the global
// `CompactionPlaceholder` advisory AND the dataflow-specific
// `CompactionOnDataflowFunction` advisory. The two are orthogonal: the
// placeholder warns the runtime is a no-op today; the dataflow warning
// pins that even when compaction lands, the config is still silently
// dropped on the dataflow arm of `FunctionRunner::run_function_with_ctx`
// because per-block conversations in `dataflow::execute_block` are
// constructed via `Conversation::new(None)` (no compaction passed in).
#[test]
fn function_level_compaction_on_dataflow_function_emits_dataflow_warning() {
    let yaml = compaction_warning_yaml(
        None,
        Some(
            "      keep_recent_tokens: 500
      reserve_tokens: 800",
        ),
        "dataflow",
    );
    load_workflow_str(&yaml).expect("load");
    let parsed = parse_workflow(&yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);

    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f"),
        1,
        "expected exactly one CompactionOnDataflowFunction warning for `f`, got: {warnings:?}"
    );
    // Placeholder still fires because the runtime compaction strategy is a
    // no-op everywhere; the dataflow warning is additional, not a
    // replacement.
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level `f`"),
        1,
        "placeholder warning must still fire for function-level config: {warnings:?}"
    );
}

// Workflow-level default `compaction:` inherited by a dataflow function
// (which declares no override of its own) still emits the dataflow
// warning, because `resolve_compaction` mirrors the runtime resolution
// path and the inherited config would also be silently dropped at runtime
// for the dataflow arm. Pins the inheritance branch of the warning.
#[test]
fn workflow_level_compaction_inherited_by_dataflow_function_emits_dataflow_warning() {
    let yaml = compaction_warning_yaml(
        Some(
            "    keep_recent_tokens: 1000
    reserve_tokens: 1000",
        ),
        None,
        "dataflow",
    );
    load_workflow_str(&yaml).expect("load");
    let parsed = parse_workflow(&yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);

    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f"),
        1,
        "expected exactly one CompactionOnDataflowFunction warning for `f` via inheritance, got: {warnings:?}"
    );
    // Workflow-level placeholder still fires (one).
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "workflow-level"),
        1
    );
    // No function-level placeholder — `f` declares no compaction of its own.
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level"),
        0
    );
}

// Imperative function with compaction: only the placeholder fires, the
// dataflow warning does NOT. Pins the negative case so a future refactor
// that conflates the two warnings would surface here. `detect_mode`
// returns `Imperative` whenever any block has a transition, so we add a
// transition to make it unambiguous.
#[test]
fn imperative_function_with_compaction_does_not_emit_dataflow_warning() {
    let yaml = compaction_warning_yaml(
        None,
        Some(
            "      keep_recent_tokens: 500
      reserve_tokens: 800",
        ),
        "imperative",
    );
    load_workflow_str(&yaml).expect("load");
    let parsed = parse_workflow(&yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);

    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f"),
        0,
        "imperative function must not emit a dataflow-discard warning, got: {warnings:?}"
    );
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level `f`"),
        1
    );
}

// Single-block function with `compaction:` and NO topology edges (no
// `transitions:`, no `depends_on:`) — `infer_mode` returns `Imperative`
// via its no-topology fallback. Pins that the dataflow-discard warning
// does NOT fire on this fallback branch (a regression that classified by
// `has_depends_on` instead of consulting `infer_mode` would pass the
// existing transition-bearing imperative test but fail this case). The
// shared `compaction_warning_yaml` helper does not support a single-block
// shape, so the YAML is inlined here.
#[test]
fn single_block_function_with_compaction_does_not_emit_dataflow_warning() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 800
    blocks:
      a:
        prompt: "only"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);

    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f"),
        0,
        "single-block function with no edges must not emit a dataflow-discard warning, got: {warnings:?}"
    );
}

// Dataflow function with NO compaction config (neither function-level nor
// workflow-level) must not emit the dataflow warning. Pins the gating
// condition so a future regression that always emits the warning whenever
// the function is dataflow would surface here.
#[test]
fn dataflow_function_without_compaction_emits_no_dataflow_warning() {
    let yaml = compaction_warning_yaml(None, None, "dataflow");
    load_workflow_str(&yaml).expect("load");
    let parsed = parse_workflow(&yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);
    assert!(
        warnings.is_empty(),
        "expected no warnings when neither workflow nor function declares compaction, got: {warnings:?}"
    );
}

// Function-level compaction on a dataflow function overrides workflow-level;
// `resolve_compaction` returns Some, so the dataflow warning fires exactly
// once for the function (not twice). The two placeholder warnings (one
// workflow, one function) still fire independently.
#[test]
fn dataflow_function_with_both_workflow_and_function_compaction_emits_one_dataflow_warning() {
    let yaml = compaction_warning_yaml(
        Some(
            "    keep_recent_tokens: 1000
    reserve_tokens: 1000",
        ),
        Some(
            "      keep_recent_tokens: 500
      reserve_tokens: 800",
        ),
        "dataflow",
    );
    load_workflow_str(&yaml).expect("load");
    let parsed = parse_workflow(&yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);

    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f"),
        1,
        "expected exactly one dataflow warning per function regardless of override layering, got: {warnings:?}"
    );
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "workflow-level"),
        1
    );
    assert_eq!(
        count_compaction_warnings_for_scope(&warnings, "function-level `f`"),
        1
    );
}

#[test]
fn compaction_on_dataflow_function_warning_display_contains_function_name_and_keywords() {
    let w = LoadWarning::CompactionOnDataflowFunction {
        function: "f1".into(),
    };
    let s = format!("{}", w);
    assert!(
        s.contains("f1"),
        "Display should mention the function name; got: {s}"
    );
    assert!(
        s.contains("dataflow") && s.contains("compaction"),
        "Display should mention both `dataflow` and `compaction`; got: {s}"
    );
}

// Two dataflow functions, each with its own `compaction:` config, must both
// emit a `CompactionOnDataflowFunction` warning. Pins the multi-function
// loop in `collect_load_warnings`: a regression that early-returned or
// deduped after the first dataflow-with-compaction function would surface
// here. The single-function tests above only exercise the loop body once.
#[test]
fn two_dataflow_functions_with_compaction_each_emit_dataflow_warning() {
    let yaml = r#"
functions:
  f1:
    input: { type: object }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 800
    blocks:
      a:
        prompt: "root1"
        schema:
          type: object
          required: [x]
          properties: { x: { type: integer } }
      b:
        prompt: "leaf1 {{block.a.output.x}}"
        schema:
          type: object
          required: [y]
          properties: { y: { type: string } }
        depends_on: [a]
  f2:
    input: { type: object }
    compaction:
      keep_recent_tokens: 600
      reserve_tokens: 900
    blocks:
      a:
        prompt: "root2"
        schema:
          type: object
          required: [x]
          properties: { x: { type: integer } }
      b:
        prompt: "leaf2 {{block.a.output.x}}"
        schema:
          type: object
          required: [y]
          properties: { y: { type: string } }
        depends_on: [a]
"#;
    load_workflow_str(yaml).expect("load");
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_load_warnings(&parsed);

    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f1"),
        1,
        "expected one CompactionOnDataflowFunction warning for `f1`, got: {warnings:?}"
    );
    assert_eq!(
        count_compaction_on_dataflow_warnings_for_function(&warnings, "f2"),
        1,
        "expected one CompactionOnDataflowFunction warning for `f2`, got: {warnings:?}"
    );
    let total_dataflow_warnings = warnings
        .iter()
        .filter(|w| matches!(w, LoadWarning::CompactionOnDataflowFunction { .. }))
        .count();
    assert_eq!(
        total_dataflow_warnings, 2,
        "expected exactly two dataflow-discard warnings (one per function), got: {warnings:?}"
    );
}
