use super::*;

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

#[test]
fn load_from_disk_roundtrips_via_tempfile() {
    let path = std::env::temp_dir().join(format!("mech-loader-test-{}.yaml", std::process::id()));
    let _ = std::fs::remove_file(&path); // clean stale

    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(path.clone());

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
