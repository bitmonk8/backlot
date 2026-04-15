//! Block definition types: [`BlockDef`], [`PromptBlock`], [`CallBlock`],
//! [`TransitionDef`], [`CallSpec`], [`CallEntry`], and [`ParallelStrategy`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Expr;
use super::agent::AgentConfigRef;
use super::schema_ref::SchemaRef;

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
    /// Returns the block's outgoing transitions.
    pub fn transitions(&self) -> &[TransitionDef] {
        match self {
            BlockDef::Prompt(p) => &p.transitions,
            BlockDef::Call(c) => &c.transitions,
        }
    }

    /// Returns the block's data-edge predecessors.
    pub fn depends_on(&self) -> &[String] {
        match self {
            BlockDef::Prompt(p) => &p.depends_on,
            BlockDef::Call(c) => &c.depends_on,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_block(transitions: Vec<TransitionDef>, depends_on: Vec<String>) -> BlockDef {
        BlockDef::Prompt(PromptBlock {
            prompt: "test".into(),
            schema: SchemaRef::Inline(serde_json::json!({"type": "object"})),
            agent: None,
            depends_on,
            set_context: Default::default(),
            set_workflow: Default::default(),
            transitions,
        })
    }

    fn call_block(transitions: Vec<TransitionDef>, depends_on: Vec<String>) -> BlockDef {
        BlockDef::Call(CallBlock {
            call: CallSpec::Single("f".into()),
            input: None,
            output: None,
            parallel: None,
            n: None,
            depends_on,
            set_context: Default::default(),
            set_workflow: Default::default(),
            transitions,
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
}
