// Reel: agent session layer.
//
// Provides a tool-loop agent runtime on top of Flick (conversation turn)
// and lot (process sandbox). Reel owns the 6 built-in tools
// (Read/Write/Edit/Glob/Grep/NuShell), the NuShell MCP session,
// and the tool loop that runs until the model returns a final response.

pub mod agent;
pub mod nu_session;
pub mod tools;

// Re-export public API types.
pub use agent::{Agent, AgentEnvironment, AgentRequestConfig, RunResult, ToolHandler, Usage};
pub use nu_session::NuSession;
/// Describes a tool's name, description, and JSON Schema parameters as seen
/// by the model.
pub use tools::ToolDefinition;
pub use tools::{ToolExecResult, ToolGrant, tool_definitions};

// Re-export flick types consumers need for building AgentEnvironment / config.
pub use flick::{ConfigFormat, ModelInfo, ModelRegistry, ProviderRegistry, RequestConfig};
/// Opaque tool configuration type. Used when injecting tools into a
/// `RequestConfig` via `add_tools()`. Convert from `ToolDefinition` using
/// `ToolConfig::new(name, description, Some(parameters))`.
pub use flick::ToolConfig;

#[cfg(any(test, feature = "testing"))]
pub mod test_support {
    pub use flick::test_support::{MultiShotProvider, SingleShotProvider};
    pub use flick::{ApiKind, DynProvider, error, provider};
}
