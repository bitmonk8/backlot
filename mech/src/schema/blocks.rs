//! Block definition types: [`BlockDef`], [`PromptBlock`], [`CallBlock`],
//! [`BlockCommon`], [`TransitionDef`], [`CallSpec`], [`CallEntry`], and
//! [`ParallelStrategy`]. Also exports the loader allow-list constants
//! [`PROMPT_BLOCK_KEYS`] and [`CALL_BLOCK_KEYS`], consumed by
//! [`crate::loader::reject_unknown_block_fields`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Expr;
use super::agent::AgentConfigRef;
use super::schema_ref::SchemaRef;

/// Whether a CEL source string is a guard expression or a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CelSourceKind {
    /// A CEL expression used in `set_context`, `set_workflow`, or `when` guards.
    Guard,
    /// A template string that may contain `{{...}}` CEL interpolations.
    Template,
}

/// Allow-list of valid YAML keys directly under a prompt block (a
/// `functions.<name>.blocks.<block_name>` entry that has a `prompt:`
/// key). Union of [`PromptBlock`]'s direct fields and the
/// [`BlockCommon`] fields it flattens. See
/// [`crate::loader::reject_unknown_block_fields`] for the rationale —
/// the loader sweep defends against the unsupported
/// `#[serde(deny_unknown_fields)]` + `#[serde(flatten)]` combination
/// (serde-rs/serde#1547).
pub const PROMPT_BLOCK_KEYS: &[&str] = &[
    "prompt",
    "schema",
    "agent",
    "depends_on",
    "set_context",
    "set_workflow",
    "transitions",
];

/// Allow-list of valid YAML keys directly under a call block (a
/// `functions.<name>.blocks.<block_name>` entry that has a `call:`
/// key). Union of [`CallBlock`]'s direct fields and the [`BlockCommon`]
/// fields it flattens. See
/// [`crate::loader::reject_unknown_block_fields`] for the rationale.
pub const CALL_BLOCK_KEYS: &[&str] = &[
    "call",
    "input",
    "output",
    "parallel",
    "n",
    "depends_on",
    "set_context",
    "set_workflow",
    "transitions",
];

/// A prompt or call block. Discrimination is by presence of `prompt` or
/// `call`. Full validity rules (§5.3) are enforced by
/// [`crate::validate::validate_workflow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlockDef {
    /// LLM-invoking block.
    Prompt(PromptBlock),
    /// Function-invoking block.
    Call(CallBlock),
}

impl BlockDef {
    /// Returns the block's shared fields (`depends_on`, `set_context`,
    /// `set_workflow`, `transitions`).
    pub fn common(&self) -> &BlockCommon {
        match self {
            BlockDef::Prompt(p) => &p.common,
            BlockDef::Call(c) => &c.common,
        }
    }

    /// Returns the block's outgoing transitions.
    pub fn transitions(&self) -> &[TransitionDef] {
        &self.common().transitions
    }

    /// Returns the block's data-edge predecessors.
    pub fn depends_on(&self) -> &[String] {
        &self.common().depends_on
    }

    /// Returns the block's `set_context` mappings.
    pub fn set_context(&self) -> &BTreeMap<String, Expr> {
        &self.common().set_context
    }

    /// Returns the block's `set_workflow` mappings.
    pub fn set_workflow(&self) -> &BTreeMap<String, Expr> {
        &self.common().set_workflow
    }

    /// Visit all CEL expression source strings in this block.
    /// Calls `visitor` with each (source_text, kind) pair.
    /// This covers: set_context values (Guard), set_workflow values (Guard),
    /// transition when clauses (Guard), prompt text (Template), call input/output
    /// mappings (Template), and per-call input mappings (Template).
    pub fn visit_cel_sources(&self, visitor: &mut impl FnMut(&str, CelSourceKind)) {
        // Shared fields across both block types
        for expr in self.set_context().values() {
            visitor(expr, CelSourceKind::Guard);
        }
        for expr in self.set_workflow().values() {
            visitor(expr, CelSourceKind::Guard);
        }
        for t in self.transitions() {
            if let Some(w) = &t.when {
                visitor(w, CelSourceKind::Guard);
            }
        }
        // Block-type-specific
        match self {
            BlockDef::Prompt(p) => {
                visitor(&p.prompt, CelSourceKind::Template);
            }
            BlockDef::Call(c) => {
                if let Some(input) = &c.input {
                    for expr in input.values() {
                        visitor(expr, CelSourceKind::Template);
                    }
                }
                if let CallSpec::PerCall(entries) = &c.call {
                    for entry in entries {
                        for expr in entry.input.values() {
                            visitor(expr, CelSourceKind::Template);
                        }
                    }
                }
                if let Some(output) = &c.output {
                    for expr in output.values() {
                        visitor(expr, CelSourceKind::Template);
                    }
                }
            }
        }
    }
}

/// Fields shared by [`PromptBlock`] and [`CallBlock`].
///
/// Embedded into [`PromptBlock`] and [`CallBlock`] with
/// `#[serde(flatten)]` so these field names appear at the block level on
/// the wire (YAML/JSON) — the representation is identical to inlining
/// `depends_on`, `set_context`, `set_workflow`, and `transitions` on
/// each block struct directly. The `round_trip_parse_reserialize_parse`
/// test pins this: a `common:` key must never appear in serialized output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlockCommon {
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

    /// Shared block fields (see [`BlockCommon`]).
    #[serde(flatten)]
    pub common: BlockCommon,
}

/// A call block (§5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallBlock {
    /// Function(s) to invoke.
    pub call: CallSpec,

    /// Shared input mapping. Required for single-function and uniform-list
    /// calls; forbidden for per-call list calls. Parse-time we accept any
    /// combination; validity is enforced by
    /// [`crate::validate::validate_workflow`].
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

    /// Shared block fields (see [`BlockCommon`]).
    #[serde(flatten)]
    pub common: BlockCommon,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_block(transitions: Vec<TransitionDef>, depends_on: Vec<String>) -> BlockDef {
        BlockDef::Prompt(PromptBlock {
            prompt: "test".into(),
            schema: SchemaRef::Inline(serde_json::json!({"type": "object"})),
            agent: None,
            common: BlockCommon {
                depends_on,
                set_context: Default::default(),
                set_workflow: Default::default(),
                transitions,
            },
        })
    }

    fn call_block(transitions: Vec<TransitionDef>, depends_on: Vec<String>) -> BlockDef {
        BlockDef::Call(CallBlock {
            call: CallSpec::Single("f".into()),
            input: None,
            output: None,
            parallel: None,
            n: None,
            common: BlockCommon {
                depends_on,
                set_context: Default::default(),
                set_workflow: Default::default(),
                transitions,
            },
        })
    }

    #[test]
    fn transitions_prompt_block() {
        let t = vec![TransitionDef {
            when: None,
            goto: "next".into(),
        }];
        let b = prompt_block(t.clone(), vec![]);
        assert_eq!(b.transitions(), &t);
    }

    #[test]
    fn transitions_call_block_empty() {
        let b = call_block(vec![], vec![]);
        assert!(b.transitions().is_empty());
    }

    #[test]
    fn depends_on_prompt_block() {
        let b = prompt_block(vec![], vec!["dep1".into(), "dep2".into()]);
        assert_eq!(b.depends_on(), &["dep1".to_string(), "dep2".to_string()]);
    }

    #[test]
    fn depends_on_call_block() {
        let b = call_block(vec![], vec!["x".into()]);
        assert_eq!(b.depends_on(), &["x".to_string()]);
    }

    #[test]
    fn visit_cel_sources_prompt_block() {
        let b = BlockDef::Prompt(PromptBlock {
            prompt: "prompt template".into(),
            schema: SchemaRef::Inline(serde_json::json!({"type": "object"})),
            agent: None,
            common: BlockCommon {
                depends_on: vec![],
                set_context: BTreeMap::from([("var1".into(), "ctx_expr".into())]),
                set_workflow: BTreeMap::from([("var2".into(), "wf_expr".into())]),
                transitions: vec![TransitionDef {
                    when: Some("guard_expr".into()),
                    goto: "next".into(),
                }],
            },
        });
        let mut sources = Vec::new();
        b.visit_cel_sources(&mut |src, kind| sources.push((src.to_string(), kind)));
        assert!(sources.contains(&("prompt template".into(), CelSourceKind::Template)));
        assert!(sources.contains(&("ctx_expr".into(), CelSourceKind::Guard)));
        assert!(sources.contains(&("wf_expr".into(), CelSourceKind::Guard)));
        assert!(sources.contains(&("guard_expr".into(), CelSourceKind::Guard)));
        assert_eq!(sources.len(), 4);
    }

    #[test]
    fn visit_cel_sources_call_block() {
        let b = BlockDef::Call(CallBlock {
            call: CallSpec::PerCall(vec![CallEntry {
                func: "f".into(),
                input: BTreeMap::from([("k".into(), "percall_expr".into())]),
            }]),
            input: Some(BTreeMap::from([("x".into(), "input_expr".into())])),
            output: Some(BTreeMap::from([("y".into(), "output_expr".into())])),
            parallel: None,
            n: None,
            common: BlockCommon {
                depends_on: vec![],
                set_context: BTreeMap::from([("c".into(), "ctx_expr2".into())]),
                set_workflow: BTreeMap::from([("w".into(), "wf_expr2".into())]),
                transitions: vec![TransitionDef {
                    when: Some("guard2".into()),
                    goto: "b".into(),
                }],
            },
        });
        let mut sources = Vec::new();
        b.visit_cel_sources(&mut |src, kind| sources.push((src.to_string(), kind)));
        assert!(sources.contains(&("input_expr".into(), CelSourceKind::Template)));
        assert!(sources.contains(&("percall_expr".into(), CelSourceKind::Template)));
        assert!(sources.contains(&("output_expr".into(), CelSourceKind::Template)));
        assert!(sources.contains(&("ctx_expr2".into(), CelSourceKind::Guard)));
        assert!(sources.contains(&("wf_expr2".into(), CelSourceKind::Guard)));
        assert!(sources.contains(&("guard2".into(), CelSourceKind::Guard)));
        assert_eq!(sources.len(), 6);
    }
}
