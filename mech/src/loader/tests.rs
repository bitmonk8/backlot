use super::*;
use crate::loader::{UnsupportedFeatureAdvisory, collect_unsupported_feature_advisories};
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

// ---- compaction-config rejection at load time --------------------------

// These tests pin that any `compaction:` config is rejected at load
// time with `MechError::UnsupportedFeature`. `UnsupportedFeatureAdvisory`
// and `collect_unsupported_feature_advisories` remain `pub` so tests can
// pin the per-scope wording without re-running the full load pipeline;
// the integration tests below additionally assert that the same wording
// reaches the `MechError::UnsupportedFeature` Display.

fn unsupported_feature_message(err: &MechError) -> String {
    match err {
        MechError::UnsupportedFeature { advisories } => advisories.join("; "),
        other => panic!("expected MechError::UnsupportedFeature, got {other:?}"),
    }
}

#[test]
fn workflow_level_compaction_rejects_load() {
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
    let err = load_workflow_str(yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("workflow-level"),
        "error must name the workflow-level scope: {msg}"
    );
    assert!(
        !msg.contains("function-level"),
        "no function-level compaction was configured; message must not mention one: {msg}"
    );

    // collect_unsupported_feature_advisories still surfaces exactly one advisory for this
    // document (the placeholder warning for the workflow-level scope).
    let parsed = parse_workflow(yaml).expect("parse");
    let warnings = collect_unsupported_feature_advisories(&parsed);
    assert_eq!(
        warnings,
        vec![UnsupportedFeatureAdvisory::CompactionUnimplemented {
            scope: "workflow-level".to_string()
        }],
        "expected exactly the workflow-level placeholder advisory, got {warnings:?}"
    );
}

#[test]
fn function_level_compaction_rejects_load() {
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
    let err = load_workflow_str(yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("function-level `f`"),
        "error must name the function-level scope: {msg}"
    );
    assert!(
        !msg.contains("workflow-level"),
        "no workflow-level compaction was configured; message must not mention one: {msg}"
    );
}

#[test]
fn combined_workflow_and_function_compaction_mentions_both_scopes() {
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
    let err = load_workflow_str(yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("workflow-level") && msg.contains("function-level `f`"),
        "error must mention both offending scopes: {msg}"
    );
}

#[test]
fn two_function_level_compactions_mention_both_functions() {
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
    let err = load_workflow_str(yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("function-level `f1`") && msg.contains("function-level `f2`"),
        "error must mention both offending function names: {msg}"
    );
}

// Helper for the dataflow vs imperative matrix. `workflow_compaction` and
// `function_compaction` are the body lines that go under `workflow.compaction:`
// (4-space indent) and `functions.f.compaction:` (6-space indent)
// respectively, or `None` to omit the section. `mode` selects the block
// topology: `"dataflow"` (b depends_on a) or `"imperative"` (a transitions
// to b).
fn compaction_advisory_yaml(
    workflow_compaction: Option<&str>,
    function_compaction: Option<&str>,
    mode: &str,
) -> String {
    let workflow_section = match workflow_compaction {
        Some(body) => format!("workflow:\n  compaction:\n{body}\n"),
        None => String::new(),
    };
    let function_compaction_section = match function_compaction {
        Some(body) => format!("    compaction:\n{body}\n"),
        None => String::new(),
    };
    let blocks = match mode {
        "dataflow" => concat!(
            "      a:\n",
            "        prompt: \"root\"\n",
            "        schema:\n",
            "          type: object\n",
            "          required: [x]\n",
            "          properties: { x: { type: integer } }\n",
            "      b:\n",
            "        prompt: \"leaf {{block.a.output.x}}\"\n",
            "        schema:\n",
            "          type: object\n",
            "          required: [y]\n",
            "          properties: { y: { type: string } }\n",
            "        depends_on: [a]",
        ),
        "imperative" => concat!(
            "      a:\n",
            "        prompt: \"step a\"\n",
            "        schema:\n",
            "          type: object\n",
            "          required: [x]\n",
            "          properties: { x: { type: string } }\n",
            "        transitions:\n",
            "          - goto: b\n",
            "      b:\n",
            "        prompt: \"step b\"\n",
            "        schema:\n",
            "          type: object\n",
            "          required: [y]\n",
            "          properties: { y: { type: string } }",
        ),
        _ => panic!("compaction_advisory_yaml: unknown mode {mode:?}"),
    };
    format!(
        "{workflow_section}functions:\n  f:\n    input: {{ type: object }}\n{function_compaction_section}    blocks:\n{blocks}\n",
    )
}

// Function-level `compaction:` declared on a dataflow function (no
// transitions, only `depends_on` edges) must still reject load, AND the
// aggregated error must include the dataflow-specific advisory in
// addition to the placeholder. The two are orthogonal: the placeholder
// rejects because the runtime is a no-op today; the dataflow advisory
// pins that even after compaction is implemented, the config would still
// be silently dropped on the dataflow arm of `FunctionRunner` because
// `dataflow::execute_block` constructs `Conversation::new(None)` per
// block. Surfacing both in the same error message preserves diagnostic
// information.
#[test]
fn dataflow_function_with_function_compaction_rejects_with_dataflow_advisory() {
    let yaml = compaction_advisory_yaml(
        None,
        Some("      keep_recent_tokens: 500\n      reserve_tokens: 800"),
        "dataflow",
    );
    let err = load_workflow_str(&yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("function-level `f`"),
        "error must name the function-level scope: {msg}"
    );
    assert!(
        msg.contains("dataflow") && msg.contains("function `f`"),
        "error must include the dataflow-specific advisory naming `f`: {msg}"
    );
}

// Workflow-level default `compaction:` inherited by a dataflow function
// (which declares no override of its own) must still surface the dataflow
// advisory in the error message, since the inherited config would also
// be silently dropped at runtime for the dataflow arm. Pins the
// inheritance branch of the advisory.
#[test]
fn dataflow_function_inheriting_workflow_compaction_rejects_with_dataflow_advisory() {
    let yaml = compaction_advisory_yaml(
        Some("    keep_recent_tokens: 1000\n    reserve_tokens: 1000"),
        None,
        "dataflow",
    );
    let err = load_workflow_str(&yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("workflow-level"),
        "error must name the workflow-level scope: {msg}"
    );
    assert!(
        msg.contains("dataflow") && msg.contains("function `f`"),
        "error must include the dataflow-specific advisory naming `f`: {msg}"
    );
    assert!(
        !msg.contains("function-level `f`"),
        "no function-level compaction was configured; placeholder advisory must not mention one: {msg}"
    );
}

// Imperative function with compaction: error contains only the
// placeholder advisory, NOT the dataflow advisory. Pins the negative
// case so a future refactor that conflated the two would surface here.
#[test]
fn imperative_function_with_compaction_rejects_without_dataflow_advisory() {
    let yaml = compaction_advisory_yaml(
        None,
        Some("      keep_recent_tokens: 500\n      reserve_tokens: 800"),
        "imperative",
    );
    let err = load_workflow_str(&yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("function-level `f`"),
        "error must name the function-level scope: {msg}"
    );
    assert!(
        !msg.contains("dataflow"),
        "imperative function must not produce a dataflow advisory: {msg}"
    );
}

// Single-block function with `compaction:` and NO topology edges (no
// `transitions:`, no `depends_on:`) — `infer_mode` returns `Imperative`
// via its no-topology fallback. Pins that the dataflow-discard advisory
// does NOT fire on this fallback branch (a regression that classified by
// `has_depends_on` instead of consulting `infer_mode` would pass the
// transition-bearing imperative test but fail this case). The shared
// `compaction_advisory_yaml` helper does not support a single-block shape,
// so the YAML is inlined here.
#[test]
fn single_block_function_with_compaction_rejects_without_dataflow_advisory() {
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
    let err = load_workflow_str(yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("function-level `f`"),
        "error must name the function-level scope: {msg}"
    );
    assert!(
        !msg.contains("dataflow"),
        "single-block function with no edges must not produce a dataflow advisory: {msg}"
    );
}

// Dataflow function with NO compaction config (neither function-level nor
// workflow-level) must still load successfully. Pins the gating
// condition so a future regression that always rejected dataflow
// functions would surface here.
#[test]
fn dataflow_function_without_compaction_loads() {
    let yaml = compaction_advisory_yaml(None, None, "dataflow");
    load_workflow_str(&yaml).expect("dataflow function with no compaction config must still load");
}

// Function-level compaction on a dataflow function overrides workflow-
// level; the dataflow advisory still fires exactly once for the
// function (not twice). Both placeholder advisories (one workflow, one
// function) appear independently, so the error message names both
// scopes plus the function once for the dataflow advisory.
#[test]
fn dataflow_function_with_both_workflow_and_function_compaction_mentions_dataflow_once() {
    let yaml = compaction_advisory_yaml(
        Some("    keep_recent_tokens: 1000\n    reserve_tokens: 1000"),
        Some("      keep_recent_tokens: 500\n      reserve_tokens: 800"),
        "dataflow",
    );
    let err = load_workflow_str(&yaml).expect_err("compaction: must reject load");
    let msg = unsupported_feature_message(&err);
    assert!(
        msg.contains("workflow-level") && msg.contains("function-level `f`"),
        "error must mention both placeholder scopes: {msg}"
    );
    // The dataflow advisory fires exactly once per function regardless
    // of whether the config came from the override or the inherited
    // default.
    let dataflow_mentions = msg.matches("function `f` is dataflow").count();
    assert_eq!(
        dataflow_mentions, 1,
        "dataflow advisory must appear exactly once for function `f`, got {dataflow_mentions} in: {msg}"
    );
}

// Pins the `UnsupportedFeatureAdvisory::CompactionOnDataflowFunction` Display so the
// per-scope wording the loader joins into the error message stays
// stable. Other tests assert what `MechError::UnsupportedFeature.message`
// contains; this test pins the source string.
#[test]
fn compaction_on_dataflow_function_advisory_display_contains_function_name_and_keywords() {
    let w = UnsupportedFeatureAdvisory::CompactionOnDataflowFunction {
        function: "f1".into(),
    };
    let s = format!("{w}");
    assert!(
        s.contains("f1"),
        "Display should mention the function name; got: {s}"
    );
    assert!(
        s.contains("dataflow") && s.contains("compaction"),
        "Display should mention both `dataflow` and `compaction`; got: {s}"
    );
}

// Pins the `UnsupportedFeatureAdvisory::CompactionUnimplemented` Display so
// the per-scope wording the loader joins into the error message stays
// stable. Sibling to
// `compaction_on_dataflow_function_advisory_display_contains_function_name_and_keywords`.
#[test]
fn compaction_unimplemented_advisory_display_contains_scope_and_keywords() {
    let w = UnsupportedFeatureAdvisory::CompactionUnimplemented {
        scope: "workflow-level".into(),
    };
    let s = format!("{w}");
    assert!(
        s.contains("workflow-level"),
        "Display should mention the scope; got: {s}"
    );
    assert!(
        s.contains("compaction") && s.contains("not implemented"),
        "Display should mention `compaction` and `not implemented`; got: {s}"
    );
}

// --------------------------------------------------------------------------
// Loader-side strict-field check (T1 / T2).
//
// See `reject_unknown_workflow_and_function_fields` for the rationale.
// These tests pin that behavior at the loader entry point.
// --------------------------------------------------------------------------

#[test]
fn rejects_unknown_field_on_workflow_flattened_subset() {
    // `systm` is a typo of `system`, which lives in the flattened
    // `ExecutionConfig` subset of `workflow:`.
    let yaml = r#"
workflow:
  systm: "You are a customer support agent."
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
"#;
    let err = load_workflow_str(yaml).expect_err("must reject typo on workflow.system");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unknown field") && msg.contains("systm") && msg.contains("workflow"),
        "error must mention 'unknown field', the offending key, and the path: {msg}"
    );
}

#[test]
fn rejects_unknown_field_on_function_flattened_subset() {
    // `compactoin` is a typo of `compaction`, in the flattened
    // `ExecutionConfig` subset of `functions.<name>:`.
    let yaml = r#"
functions:
  f:
    input: { type: object }
    compactoin:
      keep_recent_tokens: 500
      reserve_tokens: 1000
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
"#;
    let err = load_workflow_str(yaml).expect_err("must reject typo on functions.f.compaction");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unknown field") && msg.contains("compactoin") && msg.contains("functions.f"),
        "error must mention 'unknown field', the offending key, and the path: {msg}"
    );
}

#[test]
fn rejects_unknown_field_on_workflow_own_keys() {
    // `agnts` is a typo of `agents`, which is `WorkflowSection`'s own
    // (non-flattened) field. The flatten-subset allow-list still rejects
    // it because the loader-side check uses the union of own + flattened.
    let yaml = r#"
workflow:
  agnts:
    default: { model: haiku }
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
"#;
    let err = load_workflow_str(yaml).expect_err("must reject typo on workflow.agents");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unknown field") && msg.contains("agnts") && msg.contains("workflow"),
        "error must mention 'unknown field', the offending key, and the path: {msg}"
    );
}

#[test]
fn rejects_unknown_field_on_function_own_keys() {
    // `inputt` is a typo of `input`, which is `FunctionDef`'s own
    // (non-flattened) field.
    let yaml = r#"
functions:
  f:
    inputt: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
"#;
    let err = load_workflow_str(yaml).expect_err("must reject typo on functions.f.input");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unknown field") && msg.contains("inputt") && msg.contains("functions.f"),
        "error must mention 'unknown field', the offending key, and the path: {msg}"
    );
}

/// Drift guard for `WORKFLOW_SECTION_KEYS` and `FUNCTION_DEF_KEYS`.
///
/// Loads a synthetic minimal-but-complete YAML document that uses every
/// key in both allow-lists. Asserts that the loader-side strict-field
/// sweep (`reject_unknown_workflow_and_function_fields`) AND `parse_workflow`
/// both accept the document. If a future contributor adds a field to
/// `ExecutionConfig` / `WorkflowSection` / `FunctionDef` and forgets to
/// update the corresponding `*_KEYS` const, the new field's allow-list
/// entry will be missing and YAML using it will be rejected as "unknown
/// field". Adding the new field to this fixture surfaces that regression.
///
/// We invoke the strict-field sweep + parser directly rather than the full
/// `load_workflow_str` because `compaction:` (one of the keys we must
/// exercise) trips the unsupported-feature gate downstream. The sweep and
/// the parser are the only stages the allow-lists feed into.
#[test]
fn allow_lists_accept_every_documented_key() {
    let yaml = r##"
workflow:
  system: "wf system"
  agent: { model: "haiku" }
  agents:
    base: { model: "haiku" }
  context:
    wf_var: { type: integer, initial: 0 }
  schemas:
    OutSchema:
      type: object
      required: [v]
      properties:
        v: { type: string }
  compaction:
    keep_recent_tokens: 1000
    reserve_tokens: 2000
functions:
  f:
    system: "fn system"
    agent: "$ref:#base"
    context:
      fn_var: { type: integer, initial: 0 }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 1000
    input: { type: object }
    output: "infer"
    terminals: [done]
    blocks:
      done:
        prompt: "go"
        schema: "$ref:#OutSchema"
"##;
    crate::loader::reject_unknown_workflow_and_function_fields(yaml, None)
        .expect("strict-field sweep must accept every allow-listed key");
    parse_workflow(yaml).expect("serde must accept the synthetic minimal-but-complete document");
}
