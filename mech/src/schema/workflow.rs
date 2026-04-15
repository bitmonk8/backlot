//! Workflow-level types: [`WorkflowFile`], [`WorkflowDefaults`], [`FunctionDef`],
//! [`ContextVarDef`], [`CompactionConfig`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::JsonValue;
use super::agent::{AgentConfig, AgentConfigRef};
use super::blocks::BlockDef;
use super::schema_ref::SchemaRef;

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

/// A single function definition under `functions:`.
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
