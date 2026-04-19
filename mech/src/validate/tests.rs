use super::*;
use crate::schema::parse_workflow;

fn ok(yaml: &str) -> ValidationReport {
    let wf = parse_workflow(yaml).expect("yaml parses");
    validate_workflow(&wf, Some(Path::new("test.yaml")), &AnyModel)
}

fn run_with(yaml: &str, models: &dyn ModelChecker) -> ValidationReport {
    let wf = parse_workflow(yaml).expect("yaml parses");
    validate_workflow(&wf, Some(Path::new("test.yaml")), models)
}

fn assert_clean(r: &ValidationReport) {
    assert!(r.is_ok(), "expected no errors, got: {:#?}", r.errors);
}

fn assert_err_contains(r: &ValidationReport, needle: &str) {
    assert!(
        r.errors.iter().any(|e| e.message.contains(needle)),
        "no error contained `{needle}`; errors: {:#?}",
        r.errors
    );
}

// ---- Empty / structural ----

#[test]
fn rejects_empty_functions() {
    let yaml = "functions: {}\n";
    let r = ok(yaml);
    assert_err_contains(&r, "at least one function");
}

#[test]
fn passes_minimal_workflow() {
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
    assert_clean(&ok(yaml));
}

// ---- Block name format / reserved ----

#[test]
fn rejects_invalid_block_name() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      BadName:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "is not a valid identifier");
}

#[test]
fn rejects_reserved_block_name() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      input:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "reserved");
}

// `RESERVED_BLOCK_NAMES` must cover every namespace the runtime binds, or a
// block with a colliding name would silently shadow that namespace in guards
// and templates. One regression test per name (`block`, `blocks`, `meta`)
// pins coverage of the three namespaces most likely to be omitted.

#[test]
fn rejects_reserved_block_name_block() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      block:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "reserved");
}

#[test]
fn rejects_reserved_block_name_blocks() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      blocks:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "reserved");
}

#[test]
fn rejects_reserved_block_name_meta() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      meta:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "reserved");
}

// ---- Schema validity ----

#[test]
fn rejects_schema_root_not_object() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: array
          items: { type: string }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "root type must be `object`");
}

#[test]
fn rejects_schema_empty_required() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          properties: { x: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "required");
}

#[test]
fn schema_ref_resolves() {
    let yaml = r#"
workflow:
  schemas:
    res:
      type: object
      required: [ok]
      properties: { ok: { type: boolean } }
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: "$ref:#res"
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn schema_ref_unresolved() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: "$ref:#missing"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "does not resolve");
}

// ---- Context declarations ----

#[test]
fn context_var_invalid_type() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      x: { type: bogus, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "invalid JSON Schema type");
}

#[test]
fn context_var_initial_type_mismatch() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      x: { type: integer, initial: "nope" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "not compatible with declared type");
}

#[test]
fn set_context_target_must_be_declared() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        set_context:
          missing: "1"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "set_context.missing");
}

#[test]
fn set_workflow_target_must_be_declared() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        set_workflow:
          counter: "1"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "set_workflow.counter");
}

// ---- Transitions ----

#[test]
fn transition_target_must_exist() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        transitions:
          - goto: nowhere
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "does not exist");
}

#[test]
fn dead_transitions_after_fallback_warns() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - goto: b
          - goto: b
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert_clean(&r);
    assert!(!r.warnings.is_empty(), "expected dead-transition warning");
}

// ---- Dataflow cycle ----

#[test]
fn dataflow_cycle_detected() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [b]
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "dataflow cycle");
    assert_err_contains(&r, "depends on");
}

// ---- CEL compilation + variable scope ----

#[test]
fn guard_compiles_and_scope_ok() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "context.n > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn guard_invalid_cel_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "1 +"
            goto: a
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "CEL compile error");
}

#[test]
fn guard_forbids_blocks_namespace() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "blocks.a.output.k == 'x'"
            goto: a
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "not in scope inside transition `when` guards");
}

#[test]
fn template_unknown_namespace_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "{{junk.x}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "unknown variable");
}

// ---- Template reference resolution + reachability ----

#[test]
fn template_block_ref_unknown_block() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "{{blocks.nope.output.k}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "unknown block");
}

#[test]
fn template_block_ref_unknown_field() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "{{blocks.a.output.zzz}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "unknown field");
}

#[test]
fn template_block_ref_unreachable() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "{{blocks.a.output.k}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "not statically reachable");
}

#[test]
fn template_block_ref_via_depends_on_ok() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "{{blocks.a.output.k}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
    assert_clean(&ok(yaml));
}

// ---- Call blocks ----

#[test]
fn call_target_must_exist() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: nowhere
        input: { x: "y" }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "is not a function");
}

#[test]
fn call_input_required_field_missing() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: g
        input: {}
  g:
    input:
      type: object
      required: [text]
      properties: { text: { type: string } }
    blocks:
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "missing required input field `text`");
}

#[test]
fn per_call_list_must_not_have_block_input() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call:
          - fn: g
            input: { text: "x" }
        input: { text: "x" }
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      done:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "must not have a block-level `input`");
}

#[test]
fn n_of_m_requires_n() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [g, h]
        input: { text: "x" }
        parallel: n_of_m
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "requires an `n`");
}

#[test]
fn n_out_of_range() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [g, h]
        input: { text: "x" }
        parallel: n_of_m
        n: 5
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "must be in 1..=2");
}

// ---- Terminals ----

#[test]
fn explicit_terminal_must_exist() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [nope]
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "terminal block `nope`");
}

#[test]
fn explicit_terminal_must_have_no_outgoing_transitions() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [a]
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: b
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "must have no outgoing transitions");
}

// ---- Function output inference precondition ----

#[test]
fn no_terminal_blocks_with_infer_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: a
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "no terminal blocks detected");
}

// ---- Agent checks ----

#[test]
fn agent_unknown_grant_errors() {
    let yaml = r#"
workflow:
  agents:
    a:
      grant: [bogus]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "invalid grant");
}

#[test]
fn agent_unknown_model_errors() {
    let yaml = r#"
workflow:
  agents:
    a:
      model: nonesuch
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let known = KnownModels::new(["sonnet".to_string()]);
    let r = run_with(yaml, &known);
    assert_err_contains(&r, "is not known to the model registry");
}

#[test]
fn agent_extends_unknown_errors() {
    let yaml = r#"
workflow:
  agents:
    a:
      extends: missing
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "must not use `extends`");
}

#[test]
fn agent_extends_cycle_errors() {
    let yaml = r#"
workflow:
  agents:
    a:
      extends: b
    b:
      extends: a
functions:
  f:
    input: { type: object }
    blocks:
      c:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "must not use `extends`");
}

#[test]
fn named_agent_extends_rejected() {
    let yaml = r#"
workflow:
  agents:
    base:
      model: sonnet
    derived:
      extends: base
      grant: [write]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert!(!r.is_ok(), "validation must fail");
    assert_err_contains(&r, "must not use `extends`");
    assert_err_contains(&r, "derived");
}

#[test]
fn named_agent_no_extends_ok() {
    let yaml = r#"
workflow:
  agents:
    reader:
      model: sonnet
      grant: [tools]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_clean(&r);
}

#[test]
fn inline_agent_extends_still_accepted() {
    let yaml = r#"
workflow:
  agents:
    base:
      model: sonnet
      grant: [tools]
functions:
  f:
    input: { type: object }
    agent:
      extends: base
      model: opus
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        agent:
          extends: base
          grant: [write]
"#;
    let r = ok(yaml);
    assert_clean(&r);
}

#[test]
fn inline_extends_unknown_target() {
    let yaml = r#"
workflow:
  agents:
    base:
      model: sonnet
functions:
  f:
    input: { type: object }
    agent:
      extends: nonexistent
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "is not a named agent");
}

#[test]
fn named_agent_extends_cycle_detected() {
    let yaml = r#"
workflow:
  agents:
    a:
      extends: b
    b:
      extends: a
functions:
  f:
    input: { type: object }
    blocks:
      c:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "cyclic");
}

#[test]
fn agent_write_paths_without_write_grant_warns() {
    let yaml = r#"
workflow:
  agents:
    a:
      grant: [tools]
      write_paths: [src/]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_clean(&r);
    assert!(r.warnings.iter().any(|w| w.message.contains("write_paths")));
}

#[test]
fn agent_ref_unknown_named_errors() {
    let yaml = r#"
workflow:
  agents:
    a:
      model: sonnet
  agent: "$ref:#nope"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "is not a named agent");
}

#[test]
fn agent_ref_external_file_rejected() {
    // External file agent refs (e.g. `$ref:agents/reader.yaml`) are reserved
    // for future use and must produce a validation error. See MECH_SPEC §5.5.3.
    let yaml = r#"
workflow:
  agents:
    a:
      model: sonnet
  agent: "$ref:agents/reader.yaml"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "external file agent $ref");
    assert_err_contains(&r, "reserved for future use");
}

#[test]
fn agent_ref_external_file_rejected_at_function_and_block() {
    // The non-strict validation entry point (function/block-level agent refs)
    // must also reject external file refs. See MECH_SPEC §5.5.3.
    let yaml = r#"
workflow:
  agents:
    a:
      model: sonnet
  agent: "$ref:#a"
functions:
  f:
    input: { type: object }
    agent: "$ref:agents/fn_agent.yaml"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        agent: "$ref:agents/blk_agent.yaml"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "agents/fn_agent.yaml");
    assert_err_contains(&r, "agents/blk_agent.yaml");
    assert_err_contains(&r, "reserved for future use");
}

// ---- Schema ref checks ----

#[test]
fn schema_ref_external_file_rejected() {
    // External file schema refs (e.g. `$ref:schemas/resolution.json`) are
    // reserved for future use and must produce a validation error. See
    // MECH_SPEC §8.1.
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: "$ref:schemas/resolution.json"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "external file schema $ref");
    assert_err_contains(&r, "reserved for future use");
}

#[test]
fn schema_ref_external_file_rejected_for_function_output() {
    // Function-level `output:` schema refs route through the same validator;
    // external file refs are reserved for future use. See MECH_SPEC §8.1.
    let yaml = r#"
functions:
  f:
    input: { type: object }
    output: "$ref:schemas/out.yaml"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "external file schema $ref");
    assert_err_contains(&r, "reserved for future use");
}

// ---- Multiple errors collected in one pass ----

#[test]
fn collects_multiple_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: array
        transitions:
          - goto: nowhere
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "root type must be `object`");
    assert_err_contains(&r, "does not exist");
}

// ---- Unreachable block warning ----

#[test]
fn depends_on_chain_is_reachable() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
      orphan:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        depends_on: [a]
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn unreachable_block_warns() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
      orphan:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: orphan
"#;
    let r = ok(yaml);
    assert!(
        r.warnings.iter().any(|w| w.message.contains("unreachable")),
        "expected unreachable warning, got: {:#?}",
        r.warnings
    );
}

// ---- §12 worked example ----

const FULL_EXAMPLE: &str = include_str!("../../testdata/full_example.yaml");

#[test]
fn worked_example_validates_clean() {
    let wf = parse_workflow(FULL_EXAMPLE).expect("worked example parses");
    let known = KnownModels::new(["sonnet", "opus", "haiku"]);
    let r = validate_workflow(&wf, Some(Path::new("full_example.yaml")), &known);
    assert!(
        r.is_ok(),
        "worked example should validate clean, got errors: {:#?}",
        r.errors
    );
}

// ---- Source location population ----

#[test]
fn issue_location_populated_for_block_field_error() {
    let yaml = r#"
functions:
  my_fn:
    input: { type: object }
    blocks:
      b1:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - goto: nowhere
"#;
    let wf = parse_workflow(yaml).expect("yaml parses");
    let path = Path::new("workflows/test.yaml");
    let r = validate_workflow(&wf, Some(path), &AnyModel);
    let issue = r
        .errors
        .iter()
        .find(|e| e.message.contains("transition target"))
        .unwrap_or_else(|| panic!("expected transition-target error; got {:#?}", r.errors));
    assert_eq!(issue.location.file.as_deref(), Some(path));
    assert_eq!(issue.location.function.as_deref(), Some("my_fn"));
    assert_eq!(issue.location.block.as_deref(), Some("b1"));
    assert_eq!(
        issue.location.field.as_deref(),
        Some("transitions[0].goto"),
        "expected field to be populated for field-level error"
    );
}

#[test]
fn issue_location_populated_for_function_level_error() {
    let yaml = r#"
functions:
  BadFn:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let wf = parse_workflow(yaml).expect("yaml parses");
    let path = Path::new("workflows/func_err.yaml");
    let r = validate_workflow(&wf, Some(path), &AnyModel);
    let issue = r
        .errors
        .iter()
        .find(|e| e.message.contains("is not a valid identifier"))
        .unwrap_or_else(|| panic!("expected invalid-function-name error; got {:#?}", r.errors));
    assert_eq!(issue.location.file.as_deref(), Some(path));
    assert_eq!(issue.location.function.as_deref(), Some("BadFn"));
    assert_eq!(
        issue.location.block, None,
        "function-level error should have block == None"
    );
    assert_eq!(issue.location.field.as_deref(), Some("name"));
}

// ---- Spec §10.1 coverage tests ----

#[test]
fn block_with_both_prompt_and_call_rejected() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        call: other
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        input: { x: "1" }
"#;
    let err = parse_workflow(yaml).err().unwrap_or_else(|| {
        panic!(
            "expected a parse error for a block with both `prompt` and `call`; \
                 the untagged enum should fail to deserialize a block carrying both fields"
        )
    });
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unknown")
            || msg.contains("did not match")
            || msg.contains("call")
            || msg.contains("prompt"),
        "parse error should indicate the variant mismatch, got: {msg}"
    );
}

#[test]
fn block_with_neither_prompt_nor_call_rejected() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        depends_on: []
"#;
    let err = parse_workflow(yaml).err().unwrap_or_else(|| {
        panic!(
            "expected a parse error for a block with neither `prompt` nor `call`; \
                 the untagged enum should fail to deserialize a block carrying neither field"
        )
    });
    let _ = err.to_string();
}

#[test]
fn agent_grant_write_without_write_paths_clean() {
    let yaml = r#"
workflow:
  agents:
    a:
      grant: [write]
functions:
  f:
    input: { type: object }
    agent: "$ref:#a"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_clean(&r);
    assert!(
        !r.warnings.iter().any(|w| w.message.contains("write_paths")),
        "write_paths warning should NOT fire when `write` grant is present and write_paths is empty; got warnings: {:#?}",
        r.warnings
    );
}

#[test]
fn agent_empty_grant_and_write_paths_pass_validation() {
    let yaml = r#"
workflow:
  agents:
    a:
      grant: []
      write_paths: []
functions:
  f:
    input: { type: object }
    agent: "$ref:#a"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    assert_clean(&r);
    assert!(
        !r.errors.iter().any(|e| e.message.contains("invalid grant")),
        "empty grant list should not produce invalid-grant errors; got: {:#?}",
        r.errors
    );
    assert!(
        !r.warnings.iter().any(|w| w.message.contains("write_paths")),
        "empty write_paths should not trigger write_paths warning; got: {:#?}",
        r.warnings
    );
}

#[test]
fn per_call_entry_missing_fn_rejected_at_parse() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call:
          - input: { x: "1" }
"#;
    let err = parse_workflow(yaml).err().unwrap_or_else(|| {
        panic!(
            "expected parse error for per-call entry missing `fn`; parser accepted it — \
                 validator must grow an explicit check"
        )
    });
    let _ = err.to_string();
}

#[test]
fn uniform_list_call_missing_block_input_errors() {
    let yaml = r#"
functions:
  a:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  b:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  caller:
    input: { type: object }
    blocks:
      fanout:
        call: [a, b]
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "requires a block-level `input`");
}

#[test]
fn parallel_siblings_conflicting_set_context_warns() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      shared: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        set_context:
          shared: "1"
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        set_context:
          shared: "2"
"#;
    let r = ok(yaml);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.message.contains("may run in parallel") && w.message.contains("shared")),
        "expected parallel-write warning, got warnings: {:#?}",
        r.warnings
    );
}

// ---- CEL optional field safety tests -----------------------------------

#[test]
fn optional_field_safety_required_field_clean() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
        set_context:
          n: "output.category"
"#;
    let r = ok(yaml);
    assert!(
        !r.errors
            .iter()
            .any(|e| e.message.contains("optional field safety")),
        "required field access should not trigger optional field safety; errors: {:#?}",
        r.errors
    );
}

#[test]
fn optional_field_safety_optional_without_has_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        set_context:
          n: "output.x"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "optional field safety");
    assert_err_contains(&r, "output.x");
}

#[test]
fn optional_field_safety_has_guard_clean() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        set_context:
          n: "has(output.x) && output.x > 0"
"#;
    let r = ok(yaml);
    assert!(
        !r.errors
            .iter()
            .any(|e| e.message.contains("optional field safety")),
        "has()-guarded access should not trigger optional field safety; errors: {:#?}",
        r.errors
    );
}

#[test]
fn optional_field_safety_direct_has_clean() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        transitions:
          - when: "has(output.x) && output.x > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert!(
        !r.errors
            .iter()
            .any(|e| e.message.contains("optional field safety")),
        "has()-guarded access should not error; errors: {:#?}",
        r.errors
    );
}

#[test]
fn optional_field_safety_input_namespace_errors() {
    let yaml = r#"
functions:
  f:
    input:
      type: object
      required: [name]
      properties:
        name: { type: string }
        optional_field: { type: string }
    context:
      v: { type: string, initial: "" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        set_context:
          v: "input.optional_field"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "optional field safety");
    assert_err_contains(&r, "input.optional_field");
}

#[test]
fn optional_field_safety_blocks_namespace_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      v: { type: integer, initial: 0 }
    blocks:
      prev:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            opt_field: { type: integer }
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [prev]
        set_context:
          v: "blocks.prev.output.opt_field"
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "optional field safety");
    assert_err_contains(&r, "opt_field");
}

#[test]
fn optional_field_safety_nested_has_protection() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      v: { type: string, initial: "" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: object }
        set_context:
          v: "has(output.x) && output.x.y"
"#;
    let r = ok(yaml);
    assert!(
        !r.errors
            .iter()
            .any(|e| e.message.contains("optional field safety")),
        "prefix has() should protect deeper access; errors: {:#?}",
        r.errors
    );
}

#[test]
fn optional_field_safety_context_workflow_no_check() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      count: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "context.count > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert!(
        !r.errors
            .iter()
            .any(|e| e.message.contains("optional field safety")),
        "context/workflow namespaces should not trigger safety check; errors: {:#?}",
        r.errors
    );
}

#[test]
fn optional_field_safety_mixed_protected_and_unprotected() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      v: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            a: { type: integer }
            b: { type: integer }
        set_context:
          v: "has(output.a) && output.a > 0 && output.b > 0"
"#;
    let r = ok(yaml);
    let safety_errors: Vec<_> = r
        .errors
        .iter()
        .filter(|e| e.message.contains("optional field safety"))
        .collect();
    assert_eq!(
        safety_errors.len(),
        1,
        "expected exactly 1 optional field safety error, got: {:#?}",
        safety_errors
    );
    assert!(
        safety_errors[0].message.contains("output.b"),
        "error should be about output.b, got: {}",
        safety_errors[0].message
    );
}

#[test]
fn optional_field_safety_when_guard_optional_errors() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        transitions:
          - when: "output.x > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "optional field safety");
    assert_err_contains(&r, "output.x");
}
// ---- Uniform/PerCall required-field intersection tests -----------------

#[test]
fn uniform_call_optional_field_safety_fires_for_intersected_optional() {
    // Two callees: g requires [a, b], h requires [a, c].
    // Intersection of required = {a}. Accessing output.b in a
    // downstream CEL should trigger optional field safety.
    let yaml = r#"
functions:
  caller:
    input: { type: object }
    context:
      v: { type: string, initial: "" }
    blocks:
      fanout:
        call: [g, h]
        input: { text: "hello" }
        set_context:
          v: "output.b"
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    output:
      type: object
      required: [a, b]
      properties:
        a: { type: string }
        b: { type: string }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [a, b], properties: { a: { type: string }, b: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    output:
      type: object
      required: [a, c]
      properties:
        a: { type: string }
        c: { type: string }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [a, c], properties: { a: { type: string }, c: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "optional field safety");
    assert_err_contains(&r, "output.b");
}

#[test]
fn per_call_optional_field_safety_fires_for_intersected_optional() {
    // PerCall with two callees: g requires [x, y], h requires [x].
    // Intersection of required = {x}. Accessing output.y should
    // trigger optional field safety.
    let yaml = r#"
functions:
  caller:
    input: { type: object }
    context:
      v: { type: string, initial: "" }
    blocks:
      fanout:
        call:
          - fn: g
            input: { text: "a" }
          - fn: h
            input: { text: "b" }
        set_context:
          v: "output.y"
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    output:
      type: object
      required: [x, y]
      properties:
        x: { type: string }
        y: { type: string }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [x, y], properties: { x: { type: string }, y: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    output:
      type: object
      required: [x]
      properties:
        x: { type: string }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [x], properties: { x: { type: string } } }
"#;
    let r = ok(yaml);
    assert_err_contains(&r, "optional field safety");
    assert_err_contains(&r, "output.y");
}

#[test]
fn call_block_callee_no_output_schema_no_spurious_errors() {
    // Edge case: callee has no output schema. Accessing output.anything
    // should not produce spurious errors beyond the expected optional
    // field safety (there are no required fields, so all are optional).
    let yaml = r#"
functions:
  caller:
    input: { type: object }
    context:
      v: { type: string, initial: "" }
    blocks:
      c:
        call: callee
        input: { text: "hi" }
        set_context:
          v: "output.foo"
  callee:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    let r = ok(yaml);
    // No required fields available → output.foo should trigger safety
    // warning, but should NOT produce any other spurious errors.
    let non_safety_errors: Vec<_> = r
        .errors
        .iter()
        .filter(|e| !e.message.contains("optional field safety"))
        .collect();
    assert!(
        non_safety_errors.is_empty(),
        "expected no non-safety errors for callee without output schema, got: {:#?}",
        non_safety_errors
    );
}
// ---- Passing-fixture counterparts for §10.1 checks ----

#[test]
fn accepts_valid_block_name() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      good_name:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_non_reserved_block_name() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      process:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_schema_with_required_fields() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_valid_context_var() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      count: { type: integer, initial: 0 }
      name: { type: string, initial: "" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_set_context_with_declared_target() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      count: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        set_context:
          count: "1"
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_set_workflow_with_declared_target() {
    let yaml = r#"
workflow:
  context:
    counter: { type: integer, initial: 0 }
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        set_workflow:
          counter: "1"
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_valid_transition_target() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - goto: b
      b:
        prompt: "bye"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_acyclic_dataflow() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "bye"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_valid_call_target() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: g
        input: { text: "hello" }
  g:
    input:
      type: object
      required: [text]
      properties: { text: { type: string } }
    blocks:
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_valid_n_of_m() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [g, h]
        input: { text: "x" }
        parallel: n_of_m
        n: 1
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_valid_explicit_terminal() {
    let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [done]
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: done
      done:
        prompt: "bye"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_valid_agent_config() {
    let yaml = r#"
workflow:
  agents:
    reader:
      model: sonnet
      grant: [tools]
    writer:
      model: opus
      grant: [write, tools]
      write_paths: [src/]
functions:
  f:
    input: { type: object }
    agent: "$ref:#reader"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
    assert_clean(&ok(yaml));
}

#[test]
fn accepts_infer_with_terminal_blocks() {
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
    assert_clean(&ok(yaml));
}
