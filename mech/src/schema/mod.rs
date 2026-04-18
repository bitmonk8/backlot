//! Serde schema types mirroring the YAML grammar from `docs/MECH_SPEC.md` §12.
//!
//! This module is **parse-only**: it defines the struct shapes that a mech
//! workflow YAML file deserializes into, along with a thin [`parse_workflow`]
//! helper. It performs **no** semantic validation — no CEL compilation, no
//! JSON-Schema checking, no `$ref` resolution, no block-field validity rules.
//! Semantic validation lives in [`crate::validate`] and output inference in
//! [`crate::schema::infer`]; the workflow loader that ties everything together
//! lands in a later deliverable.
//!
//! Every struct uses `#[serde(deny_unknown_fields)]` so that typos and
//! accidental fields are caught at load time.
//!
//! # Design notes
//!
//! Several fields in the YAML grammar are polymorphic. They are modelled with
//! `#[serde(untagged)]` enums:
//!
//! * [`SchemaRef`] — inline JSON Schema object, `$ref:...` string, or the
//!   literal `"infer"`.
//! * [`AgentConfigRef`] — inline [`AgentConfig`] object or `$ref:...` string.
//! * [`CallSpec`] — single function name, a uniform list of names, or a
//!   per-call list of `{ fn, input }` entries.
//!
//! [`BlockDef`] is also an untagged enum distinguishing prompt blocks from
//! call blocks by the presence of the `prompt` vs `call` field. Full field
//! validity (e.g. "a prompt block must not have `call`") is enforced by
//! [`crate::validate::validate_workflow`]; this module only rejects genuinely
//! unknown fields.
//!
//! ## Naming convention
//!
//! Types that represent keyed definition entries in YAML maps carry a `Def`
//! suffix (`FunctionDef`, `BlockDef`, `TransitionDef`, `ContextVarDef`).
//! Structural/config types that appear as fields within definitions do not
//! (`PromptBlock`, `CallBlock`, `AgentConfig`, `CompactionConfig`).

mod agent;
mod blocks;
pub mod infer;
mod mode;
pub mod registry;
mod schema_ref;
mod workflow;

pub use agent::*;
pub use blocks::*;
pub use schema_ref::*;
pub use workflow::*;

pub use infer::infer_function_outputs;
pub(crate) use mode::{InferMode, infer_mode};
pub use registry::{
    ResolvedSchema, SchemaRegistry, parse_named_ref, resolve_schema_ref_in_map,
    resolve_schema_value, try_parse_named_ref, value_matches_json_type,
};

/// A raw, template-bearing string (CEL / `{{...}}`). Kept as-is at parse time.
pub type Expr = String;

/// A raw JSON Schema value. JSON Schema compilation and validation live in
/// [`crate::schema::registry`]; this type alias remains `serde_json::Value`
/// because inline schemas in the YAML grammar can be any JSON value.
pub type JsonValue = serde_json::Value;

/// Parse a [`MechDocument`] from a YAML string.
///
/// This is a thin wrapper around `serde_yml::from_str` that exists to give the
/// rest of the crate a single entry point. No semantic validation is
/// performed.
pub fn parse_workflow(yaml: &str) -> Result<MechDocument, serde_yml::Error> {
    serde_yml::from_str(yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The complete §12.2 worked example. Kept in-file so tests are
    // self-contained and fail loudly if the spec drifts.
    const FULL_EXAMPLE: &str = include_str!("../../testdata/full_example.yaml");

    #[test]
    fn parses_full_worked_example() {
        let wf = parse_workflow(FULL_EXAMPLE).expect("full example must parse");

        let defaults = wf.workflow.as_ref().expect("workflow defaults present");
        assert_eq!(
            defaults.system.as_deref(),
            Some("You are a customer support agent.")
        );
        assert!(defaults.agents.contains_key("default"));
        assert!(defaults.agents.contains_key("diagnostician"));
        match &defaults.agent {
            Some(AgentConfigRef::Ref(s)) => assert_eq!(s, "$ref:#default"),
            other => panic!("expected workflow.agent = $ref:#default, got {other:?}"),
        }
        assert!(defaults.schemas.contains_key("resolution"));

        let triage = wf
            .functions
            .get("support_triage")
            .expect("support_triage present");
        assert!(triage.context.contains_key("attempts"));
        assert_eq!(triage.context["attempts"].ty, "integer");

        // classify is a prompt block with transitions.
        let classify = triage.blocks.get("classify").expect("classify block");
        match classify {
            BlockDef::Prompt(p) => {
                assert!(p.prompt.contains("Classify"));
                assert_eq!(p.transitions.len(), 3);
                assert_eq!(p.transitions[2].goto, "general");
                assert!(p.transitions[2].when.is_none());
            }
            _ => panic!("classify must be a prompt block"),
        }

        // billing is a call block (single function).
        let billing = triage.blocks.get("billing").expect("billing block");
        match billing {
            BlockDef::Call(c) => match &c.call {
                CallSpec::Single(name) => assert_eq!(name, "resolve_billing"),
                other => panic!("expected single call, got {other:?}"),
            },
            _ => panic!("billing must be a call block"),
        }

        // technical uses an $ref agent and has a self-loop transition.
        let technical = triage.blocks.get("technical").expect("technical block");
        match technical {
            BlockDef::Prompt(p) => {
                match &p.agent {
                    Some(AgentConfigRef::Ref(s)) => assert_eq!(s, "$ref:#diagnostician"),
                    other => panic!("expected $ref agent, got {other:?}"),
                }
                assert!(p.set_context.contains_key("attempts"));
            }
            _ => panic!("technical must be a prompt block"),
        }

        // general uses a schema $ref.
        let general = triage.blocks.get("general").expect("general block");
        if let BlockDef::Prompt(p) = general {
            match &p.schema {
                SchemaRef::Ref(s) => assert_eq!(s, "$ref:#resolution"),
                other => panic!("expected schema $ref, got {other:?}"),
            }
        } else {
            panic!("general must be prompt");
        }

        // resolve_billing uses an inline agent with extends.
        let rb = wf
            .functions
            .get("resolve_billing")
            .expect("resolve_billing present");
        match &rb.agent {
            Some(AgentConfigRef::Inline(a)) => {
                assert_eq!(a.extends.as_deref(), Some("default"));
                assert_eq!(a.grants, Some(vec!["write".to_string()]));
                assert_eq!(a.write_paths, Some(vec!["billing/".to_string()]));
            }
            other => panic!("expected inline agent with extends, got {other:?}"),
        }
    }

    #[test]
    fn parses_call_input_single_string_form() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: other
        input:
          text: "{{input.text}}"
"#;
        let wf = parse_workflow(yaml).unwrap();
        let block = &wf.functions["f"].blocks["b"];
        match block {
            BlockDef::Call(c) => {
                assert!(matches!(&c.call, CallSpec::Single(s) if s == "other"));
                assert!(c.input.as_ref().unwrap().contains_key("text"));
            }
            _ => panic!("expected call block"),
        }
    }

    #[test]
    fn parses_call_uniform_list_form() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [a, b, c]
        input: { x: "{{input.x}}" }
        parallel: all
"#;
        let wf = parse_workflow(yaml).unwrap();
        let block = &wf.functions["f"].blocks["b"];
        match block {
            BlockDef::Call(c) => {
                match &c.call {
                    CallSpec::Uniform(v) => {
                        assert_eq!(v, &vec!["a".to_string(), "b".into(), "c".into()])
                    }
                    other => panic!("expected uniform list, got {other:?}"),
                }
                assert_eq!(c.parallel, Some(ParallelStrategy::All));
            }
            _ => panic!("expected call block"),
        }
    }

    #[test]
    fn parses_call_per_call_object_list_form() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call:
          - fn: sentiment_check
            input: { text: "{{input.text}}" }
          - fn: policy_lookup
            input: { query: "{{input.text}}" }
        parallel: n_of_m
        n: 1
"#;
        let wf = parse_workflow(yaml).unwrap();
        let block = &wf.functions["f"].blocks["b"];
        match block {
            BlockDef::Call(c) => {
                match &c.call {
                    CallSpec::PerCall(v) => {
                        assert_eq!(v.len(), 2);
                        assert_eq!(v[0].func, "sentiment_check");
                        assert!(v[1].input.contains_key("query"));
                    }
                    other => panic!("expected per-call list, got {other:?}"),
                }
                assert_eq!(c.parallel, Some(ParallelStrategy::NOfM));
                assert_eq!(c.n, Some(1));
            }
            _ => panic!("expected call block"),
        }
    }

    #[test]
    fn parses_schema_inline_variant() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        let wf = parse_workflow(yaml).unwrap();
        if let BlockDef::Prompt(p) = &wf.functions["f"].blocks["b"] {
            assert!(matches!(p.schema, SchemaRef::Inline(_)));
        } else {
            panic!("expected prompt");
        }
    }

    #[test]
    fn parses_schema_ref_variant() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: "$ref:#resolution"
"#;
        let wf = parse_workflow(yaml).unwrap();
        if let BlockDef::Prompt(p) = &wf.functions["f"].blocks["b"] {
            match &p.schema {
                SchemaRef::Ref(s) => assert_eq!(s, "$ref:#resolution"),
                other => panic!("expected schema ref, got {other:?}"),
            }
        } else {
            panic!("expected prompt");
        }
    }

    #[test]
    fn parses_function_output_infer_literal() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
"#;
        let wf = parse_workflow(yaml).unwrap();
        match &wf.functions["f"].output {
            Some(SchemaRef::Infer) => {}
            other => panic!("expected Infer, got {other:?}"),
        }
    }

    #[test]
    fn parses_agent_cascade_at_all_three_levels() {
        let yaml = r#"
workflow:
  agent:
    model: haiku
    grant: [tools]
functions:
  f:
    input: { type: object }
    agent: "$ref:#base"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
        agent:
          extends: base
          model: opus
"#;
        let wf = parse_workflow(yaml).unwrap();

        // workflow-level: inline
        match &wf.workflow.as_ref().unwrap().agent {
            Some(AgentConfigRef::Inline(a)) => {
                assert_eq!(a.model.as_deref(), Some("haiku"));
                assert_eq!(a.grants, Some(vec!["tools".to_string()]));
            }
            other => panic!("expected inline agent at workflow level, got {other:?}"),
        }

        // function-level: $ref
        let f = &wf.functions["f"];
        match &f.agent {
            Some(AgentConfigRef::Ref(s)) => assert_eq!(s, "$ref:#base"),
            other => panic!("expected function-level $ref agent, got {other:?}"),
        }

        // block-level: inline with extends
        if let BlockDef::Prompt(p) = &f.blocks["b"] {
            match &p.agent {
                Some(AgentConfigRef::Inline(a)) => {
                    assert_eq!(a.extends.as_deref(), Some("base"));
                    assert_eq!(a.model.as_deref(), Some("opus"));
                }
                other => panic!("expected block-level inline agent, got {other:?}"),
            }
        } else {
            panic!("expected prompt block");
        }
    }

    #[test]
    fn round_trip_parse_reserialize_parse() {
        let wf1 = parse_workflow(FULL_EXAMPLE).expect("parse 1");
        let reserialized = serde_yml::to_string(&wf1).expect("reserialize");
        let wf2 = parse_workflow(&reserialized).expect("parse 2");
        assert_eq!(
            wf1, wf2,
            "round-trip must be a fixed point on the struct model"
        );
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
gremlins: 3
"#;
        let err = parse_workflow(yaml).expect_err("must reject unknown top-level field");
        let msg = err.to_string();
        assert!(
            msg.contains("gremlins") || msg.to_lowercase().contains("unknown"),
            "error should mention the unknown field: {msg}"
        );
    }

    #[test]
    fn rejects_unknown_block_field() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
        bogus: true
"#;
        let err = parse_workflow(yaml).expect_err("must reject unknown block field");
        let msg = err.to_string().to_lowercase();
        // Because BlockDef is an untagged enum, the error won't always name
        // the field exactly, but serde's message typically mentions either the
        // offending field name or indicates that no variant matched.
        assert!(
            msg.contains("bogus") || msg.contains("did not match") || msg.contains("unknown"),
            "error should indicate rejection due to unknown block field: {msg}"
        );
    }

    #[test]
    fn parses_workflow_context_declarations() {
        let yaml = r#"
workflow:
  context:
    total_calls: { type: integer, initial: 0 }
    all_categories: { type: array, initial: [] }
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
"#;
        let wf = parse_workflow(yaml).unwrap();
        let ctx = &wf.workflow.as_ref().unwrap().context;
        assert_eq!(ctx["total_calls"].ty, "integer");
        assert_eq!(ctx["total_calls"].initial, serde_json::json!(0));
        assert_eq!(ctx["all_categories"].ty, "array");
        assert_eq!(ctx["all_categories"].initial, serde_json::json!([]));
    }

    #[test]
    fn parses_compaction_and_terminals_and_set_context() {
        let yaml = r#"
workflow:
  compaction:
    keep_recent_tokens: 2000
    reserve_tokens: 4000
    fn: my_compactor
functions:
  f:
    input: { type: object }
    terminals: [done]
    context:
      attempts: { type: integer, initial: 0 }
    compaction:
      keep_recent_tokens: 500
      reserve_tokens: 1000
    blocks:
      done:
        prompt: "hi"
        schema: { type: object }
        depends_on: []
        set_context:
          attempts: "context.attempts + 1"
        set_workflow: {}
"#;
        let wf = parse_workflow(yaml).unwrap();
        let wdef = wf.workflow.as_ref().unwrap();
        let comp = wdef.compaction.as_ref().unwrap();
        assert_eq!(comp.keep_recent_tokens, 2000);
        assert_eq!(comp.reserve_tokens, 4000);
        assert_eq!(comp.func.as_deref(), Some("my_compactor"));

        let f = &wf.functions["f"];
        assert_eq!(f.terminals, vec!["done".to_string()]);
        assert!(f.context.contains_key("attempts"));
        assert_eq!(f.compaction.as_ref().unwrap().keep_recent_tokens, 500);

        if let BlockDef::Prompt(p) = &f.blocks["done"] {
            assert_eq!(
                p.set_context.get("attempts").map(String::as_str),
                Some("context.attempts + 1")
            );
        } else {
            panic!("expected prompt");
        }
    }

    #[test]
    fn parses_call_block_output_mapping() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: other
        input: { text: "{{input.text}}" }
        output:
          result: "{{call.result}}"
          score: "{{call.score}}"
"#;
        let wf = parse_workflow(yaml).unwrap();
        if let BlockDef::Call(c) = &wf.functions["f"].blocks["b"] {
            let out = c.output.as_ref().expect("output mapping present");
            assert_eq!(
                out.get("result").map(String::as_str),
                Some("{{call.result}}")
            );
            assert_eq!(out.get("score").map(String::as_str), Some("{{call.score}}"));
        } else {
            panic!("expected call block");
        }
    }

    #[test]
    fn parses_transition_with_when_guard() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object }
        transitions:
          - when: "output.category == 'billing'"
            goto: billing
"#;
        let wf = parse_workflow(yaml).unwrap();
        if let BlockDef::Prompt(p) = &wf.functions["f"].blocks["b"] {
            assert_eq!(p.transitions.len(), 1);
            assert_eq!(
                p.transitions[0].when.as_deref(),
                Some("output.category == 'billing'")
            );
            assert_eq!(p.transitions[0].goto, "billing");
        } else {
            panic!("expected prompt");
        }
    }

    #[test]
    fn parses_parallel_strategy_any() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [a, b]
        input: { x: "{{input.x}}" }
        parallel: any
"#;
        let wf = parse_workflow(yaml).unwrap();
        if let BlockDef::Call(c) = &wf.functions["f"].blocks["b"] {
            assert_eq!(c.parallel, Some(ParallelStrategy::Any));
        } else {
            panic!("expected call block");
        }
    }

    #[test]
    fn parses_schema_inline_body_contents() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        let wf = parse_workflow(yaml).unwrap();
        if let BlockDef::Prompt(p) = &wf.functions["f"].blocks["b"] {
            match &p.schema {
                SchemaRef::Inline(v) => {
                    assert_eq!(v.get("type").and_then(|t| t.as_str()), Some("object"));
                    assert!(v.get("properties").and_then(|p| p.get("answer")).is_some());
                }
                other => panic!("expected inline schema, got {other:?}"),
            }
        } else {
            panic!("expected prompt");
        }
    }
}
