//! Workflow-level types: [`MechDocument`], [`WorkflowSection`], [`FunctionDef`], [`ContextVarDef`], [`CompactionConfig`], plus the cross-cutting [`ExecutionConfig`] embedded in both [`WorkflowSection`] and [`FunctionDef`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::JsonValue;
use super::agent::{AgentConfig, AgentConfigRef};
use super::blocks::BlockDef;
use super::schema_ref::SchemaRef;

/// Root document: a parsed mech workflow file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MechDocument {
    /// Workflow-level defaults (system prompt, agents, context, schemas, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowSection>,

    /// Function definitions, keyed by function name.
    pub functions: BTreeMap<String, FunctionDef>,
}

/// Contents of the top-level `workflow:` block.
///
/// The four fields shared with [`FunctionDef`] (system prompt, agent, context
/// variable declarations, and compaction config) are reached via the embedded
/// [`ExecutionConfig`] in `defaults`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSection {
    /// Shared workflow-level defaults: system prompt, agent config, context
    /// variable declarations, and conversation compaction.
    #[serde(flatten)]
    pub defaults: ExecutionConfig,

    /// Named, reusable agent configurations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, AgentConfig>,

    /// Named, reusable JSON-Schema definitions (values are raw JSON).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub schemas: BTreeMap<String, JsonValue>,
}

/// A single function definition under `functions:`.
///
/// The four fields shared with [`WorkflowSection`] (system prompt, agent,
/// context variable declarations, and compaction config) are reached via the
/// embedded [`ExecutionConfig`] in `overrides` — at function scope these
/// values *supersede* the workflow-level [`WorkflowSection::defaults`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionDef {
    /// Input JSON Schema (root type: object).
    pub input: JsonValue,

    /// Output schema: inline, `$ref:...`, or the literal `"infer"`. Defaults
    /// to `"infer"` if omitted (resolved by
    /// [`crate::schema::infer::infer_function_outputs`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<SchemaRef>,

    /// Function-level overrides for system prompt, agent config, context
    /// variable declarations, and conversation compaction. See
    /// [`ExecutionConfig`] for per-field replace-vs-merge semantics —
    /// `system`/`agent`/`compaction` replace, `context` merges.
    #[serde(flatten)]
    pub overrides: ExecutionConfig,

    /// Explicit terminal block names. Auto-detected when omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<String>,

    /// Block definitions, keyed by block name.
    pub blocks: BTreeMap<String, BlockDef>,
}

/// Fields shared by [`WorkflowSection`] and [`FunctionDef`]: workflow-level
/// defaults / function-level overrides for system prompt, agent config,
/// context variable declarations, and conversation compaction.
///
/// Per-field precedence semantics when both workflow and function scopes
/// declare a value:
///
/// * `system`, `agent`, `compaction` — **replace**: the function-scope value,
///   if present, fully replaces the workflow-scope value (see the
///   `resolved_*` accessors below).
/// * `context` — **merge**: function-scope declarations are added to the
///   workflow-scope set; both scopes contribute (see consumers in `exec/`).
///
/// See [`crate::loader::reject_unknown_workflow_and_function_fields`] for
/// the loader-side strict-key check that compensates for serde-flatten
/// disabling `deny_unknown_fields` on the embedding parents.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    /// System prompt (template string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    /// Agent config (inline or `$ref:...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfigRef>,

    /// Context variable declarations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, ContextVarDef>,

    /// Conversation compaction config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionConfig>,
}

impl ExecutionConfig {
    /// Resolve the effective `system` value: prefer this scope's value
    /// (function-level override), fall back to `parent`'s value
    /// (workflow-level default).
    pub fn resolved_system<'a>(&'a self, parent: Option<&'a ExecutionConfig>) -> Option<&'a str> {
        self.system
            .as_deref()
            .or_else(|| parent.and_then(|p| p.system.as_deref()))
    }

    /// Resolve the effective `agent` value: prefer this scope's value
    /// (function-level override), fall back to `parent`'s value
    /// (workflow-level default).
    pub fn resolved_agent<'a>(
        &'a self,
        parent: Option<&'a ExecutionConfig>,
    ) -> Option<&'a AgentConfigRef> {
        self.agent
            .as_ref()
            .or_else(|| parent.and_then(|p| p.agent.as_ref()))
    }

    /// Resolve the effective `compaction` value: prefer this scope's value
    /// (function-level override), fall back to `parent`'s value
    /// (workflow-level default).
    pub fn resolved_compaction<'a>(
        &'a self,
        parent: Option<&'a ExecutionConfig>,
    ) -> Option<&'a CompactionConfig> {
        self.compaction
            .as_ref()
            .or_else(|| parent.and_then(|p| p.compaction.as_ref()))
    }

    // No `resolved_context` accessor: `context` maps are *merged* across
    // scopes (workflow + function), not cascaded with replace semantics
    // like the three fields above. Existing call sites iterate both maps
    // and the merge semantics differ per consumer (declaration union vs.
    // initial-value precedence), so a single resolver would obscure rather
    // than clarify the intent.
}

/// Allow-list of valid YAML keys directly under top-level `workflow:`.
/// Union of [`ExecutionConfig`]'s keys and [`WorkflowSection`]'s own keys
/// (`agents`, `schemas`). See
/// [`crate::loader::reject_unknown_workflow_and_function_fields`] for the
/// rationale.
pub const WORKFLOW_SECTION_KEYS: &[&str] = &[
    "system",
    "agent",
    "context",
    "compaction",
    "agents",
    "schemas",
];

/// Allow-list of valid YAML keys directly under each `functions.<name>:` entry.
/// Union of [`ExecutionConfig`]'s keys and [`FunctionDef`]'s own keys
/// (`input`, `output`, `terminals`, `blocks`). See
/// [`crate::loader::reject_unknown_workflow_and_function_fields`] for the
/// rationale.
pub const FUNCTION_DEF_KEYS: &[&str] = &[
    "system",
    "agent",
    "context",
    "compaction",
    "input",
    "output",
    "terminals",
    "blocks",
];

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
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "fn")]
    pub func: Option<String>,
}
