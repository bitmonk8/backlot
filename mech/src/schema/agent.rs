//! Agent configuration types: [`AgentConfigRef`] and [`AgentConfig`].

use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "grant")]
    pub grants: Option<Vec<String>>,

    /// Custom tool names (must be registered with the executor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,

    /// Writable paths (relative to project root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_paths: Option<Vec<String>>,

    /// Agent-run timeout (e.g. `"30s"`, `"5m"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Name of a workflow-level named agent config to use as a base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
}

impl AgentConfig {
    /// Returns the grants list, or an empty slice if unset.
    pub fn grants_list(&self) -> &[String] {
        self.grants.as_deref().unwrap_or_default()
    }

    /// Returns the tools list, or an empty slice if unset.
    pub fn tool_list(&self) -> &[String] {
        self.tools.as_deref().unwrap_or_default()
    }

    /// Returns the write_paths list, or an empty slice if unset.
    pub fn write_path_list(&self) -> &[String] {
        self.write_paths.as_deref().unwrap_or_default()
    }
}
