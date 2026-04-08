//! Serde schema types mirroring the YAML grammar from `docs/MECH_SPEC.md` §12.
//!
//! This module is **parse-only**: it defines the struct shapes that a mech
//! workflow YAML file deserializes into, along with a thin [`parse_workflow`]
//! helper. It performs **no** semantic validation — no CEL compilation, no
//! JSON-Schema checking, no `$ref` resolution, no block-field validity rules.
//! Those live in later deliverables (Deliverables 3–7).
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
//! validity (e.g. "a prompt block must not have `call`") is enforced later in
//! Deliverable 5; this module only rejects genuinely unknown fields.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A raw, template-bearing string (CEL / `{{...}}`). Kept as-is at parse time.
pub type Expr = String;

/// A raw JSON Schema value. We defer JSON-Schema validation to Deliverable 4,
/// so any valid JSON value is accepted here.
pub type JsonValue = serde_json::Value;

// ─── Top-level ──────────────────────────────────────────────────────────────

/// Root document: a parsed mech workflow file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFile {
    /// Workflow-level defaults (system prompt, agents, context, schemas, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowDefaults>,

    /// Function definitions, keyed by function name.
    pub functions: BTreeMap<String, FunctionDef>,
}

/// Contents of the top-level `workflow:` block.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefaults {
    /// Default system prompt (template string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    /// Default agent config (inline or `$ref:...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfigRef>,

    /// Named, reusable agent configurations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, AgentConfig>,

    /// Workflow-level context variable declarations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, ContextVarDef>,

    /// Named, reusable JSON-Schema definitions (values are raw JSON).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub schemas: BTreeMap<String, JsonValue>,

    /// Default conversation compaction config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionConfig>,
}

// ─── Functions ──────────────────────────────────────────────────────────────

/// A single function definition under `functions:`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionDef {
    /// Input JSON Schema (root type: object).
    pub input: JsonValue,

    /// Output schema: inline, `$ref:...`, or the literal `"infer"`. Defaults
    /// to `"infer"` if omitted (handled by Deliverable 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<SchemaRef>,

    /// Function-level system prompt override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    /// Function-level agent config override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfigRef>,

    /// Explicit terminal block names. Auto-detected when omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<String>,

    /// Function-level context variable declarations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, ContextVarDef>,

    /// Compaction override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionConfig>,

    /// Block definitions, keyed by block name.
    pub blocks: BTreeMap<String, BlockDef>,
}

// ─── Blocks ─────────────────────────────────────────────────────────────────

/// A prompt or call block. Discrimination is by presence of `prompt` or
/// `call`. Full validity rules (§5.3) are enforced in Deliverable 5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlockDef {
    /// LLM-invoking block.
    Prompt(PromptBlock),
    /// Function-invoking block.
    Call(CallBlock),
}

/// A prompt block (§5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptBlock {
    /// Prompt template string.
    pub prompt: String,

    /// Output schema (inline or `$ref:...`).
    pub schema: SchemaRef,

    /// Optional agent config override for this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfigRef>,

    /// Data-edge predecessors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Writes to function context variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_context: BTreeMap<String, Expr>,

    /// Writes to workflow context variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_workflow: BTreeMap<String, Expr>,

    /// Outbound control edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<TransitionDef>,
}

/// A call block (§5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallBlock {
    /// Function(s) to invoke.
    pub call: CallSpec,

    /// Shared input mapping. Required for single-function and uniform-list
    /// calls; forbidden for per-call list calls. Parse-time we accept any
    /// combination; validity is enforced in Deliverable 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<BTreeMap<String, Expr>>,

    /// Output mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<BTreeMap<String, Expr>>,

    /// Parallel join strategy for list calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<ParallelStrategy>,

    /// Required completions for `n_of_m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// Data-edge predecessors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Writes to function context variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_context: BTreeMap<String, Expr>,

    /// Writes to workflow context variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set_workflow: BTreeMap<String, Expr>,

    /// Outbound control edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<TransitionDef>,
}

/// The three shapes of `call:` (§4.4, §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CallSpec {
    /// Single function name: `call: my_fn`.
    Single(String),
    /// Uniform list (shared input): `call: [a, b, c]`.
    Uniform(Vec<String>),
    /// Per-call list: `call: [{ fn: a, input: { ... } }, ...]`.
    PerCall(Vec<CallEntry>),
}

/// A single entry in a per-call list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallEntry {
    /// Function name.
    #[serde(rename = "fn")]
    pub func: String,
    /// Input mapping for this specific call.
    pub input: BTreeMap<String, Expr>,
}

/// Parallel join strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelStrategy {
    /// Wait for every call to complete.
    All,
    /// Resume as soon as any call completes.
    Any,
    /// Resume when `n` of the calls complete.
    NOfM,
}

/// A single outbound control edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionDef {
    /// Optional CEL guard (`when:`). Omit for an unconditional edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Expr>,
    /// Target block name.
    pub goto: String,
}

// ─── Agent configuration ────────────────────────────────────────────────────

/// `agent:` reference at any level: an inline [`AgentConfig`] or a `$ref:...`
/// string (`$ref:#name` or `$ref:path`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentConfigRef {
    /// `$ref:#name` or `$ref:path`.
    Ref(String),
    /// Inline agent configuration object.
    Inline(AgentConfig),
}

/// Inline agent configuration (§5.5.1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// flick model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// ToolGrant flags (`tools`, `write`, `network`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grant: Vec<String>,

    /// Custom tool names (must be registered with the executor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,

    /// Writable paths (relative to project root).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_paths: Vec<String>,

    /// Agent-run timeout (e.g. `"30s"`, `"5m"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Name of a workflow-level named agent config to use as a base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
}

// ─── Schemas ────────────────────────────────────────────────────────────────

/// A JSON Schema reference: inline, external/named ref, or the literal
/// `"infer"` (function output only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaRef {
    /// The string `"infer"` — requests automatic inference (function output).
    Infer(InferLiteral),
    /// `$ref:#name` or `$ref:path`.
    Ref(String),
    /// Inline JSON Schema object.
    Inline(JsonValue),
}

/// Serialises as the literal string `"infer"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InferLiteral {
    /// The one and only inhabitant.
    #[serde(rename = "infer")]
    Infer,
}

// ─── Context & misc ─────────────────────────────────────────────────────────

/// A single context variable declaration (§9.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextVarDef {
    /// JSON Schema type name (`string`, `number`, `integer`, `boolean`,
    /// `array`, `object`).
    #[serde(rename = "type")]
    pub ty: String,
    /// Literal initial value, compatible with `ty`.
    pub initial: JsonValue,
}

/// Conversation compaction configuration (§4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// Tokens of recent history to preserve verbatim.
    pub keep_recent_tokens: u32,
    /// Trigger threshold (fire when `used > context_window - reserve`).
    pub reserve_tokens: u32,
    /// Optional custom compaction function name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#fn: Option<String>,
}

// ─── Parse helper ───────────────────────────────────────────────────────────

/// Parse a [`WorkflowFile`] from a YAML string.
///
/// This is a thin wrapper around `serde_yml::from_str` that exists to give the
/// rest of the crate a single entry point. No semantic validation is
/// performed.
pub fn parse_workflow(yaml: &str) -> Result<WorkflowFile, serde_yml::Error> {
    serde_yml::from_str(yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The complete §12.2 worked example. Kept in-file so tests are
    // self-contained and fail loudly if the spec drifts.
    const FULL_EXAMPLE: &str = include_str!("full_example.yaml");

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
                assert_eq!(a.grant, vec!["write".to_string()]);
                assert_eq!(a.write_paths, vec!["billing/".to_string()]);
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
            Some(SchemaRef::Infer(InferLiteral::Infer)) => {}
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
                assert_eq!(a.grant, vec!["tools".to_string()]);
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
        assert_eq!(comp.r#fn.as_deref(), Some("my_compactor"));

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
