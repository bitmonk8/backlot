// Tool access flags: READ, WRITE, BASH, TASK, WEB presets.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::Path;

bitflags! {
    /// Permission flags controlling which tools an agent call may use.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ToolGrant: u8 {
        const READ  = 0b0000_0001;
        const WRITE = 0b0000_0010;
        const BASH  = 0b0000_0100;
        const TASK  = 0b0000_1000;
        const WEB   = 0b0001_0000;
    }
}

/// Method categories that map to different tool grant sets.
#[derive(Debug, Clone, Copy)]
pub enum AgentMethod {
    /// Assessment / verification / checkpoint / recovery — read-only analysis.
    Analyze,
    /// Leaf execution — needs read, write, bash.
    Execute,
    /// Design and decompose — needs read for exploration.
    Decompose,
}

/// Returns the tool grant set appropriate for a given agent method.
pub fn phase_tools(method: AgentMethod) -> ToolGrant {
    match method {
        AgentMethod::Execute => ToolGrant::READ | ToolGrant::WRITE | ToolGrant::BASH,
        AgentMethod::Analyze | AgentMethod::Decompose => ToolGrant::READ,
    }
}

/// A tool definition suitable for inclusion in a Flick config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlickToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

/// Returns tool definitions for all tools permitted by the given grant.
pub fn tool_definitions(grant: ToolGrant) -> Vec<FlickToolDef> {
    let mut tools = Vec::new();

    if grant.contains(ToolGrant::READ) {
        tools.push(FlickToolDef {
            name: "read_file".into(),
            description: "Read the contents of a file at the given path.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or project-relative file path" }
                },
                "required": ["path"]
            }),
        });
        tools.push(FlickToolDef {
            name: "glob".into(),
            description: "Find files matching a glob pattern.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs)" },
                    "path": { "type": "string", "description": "Directory to search in (defaults to project root)" }
                },
                "required": ["pattern"]
            }),
        });
        tools.push(FlickToolDef {
            name: "grep".into(),
            description: "Search file contents for a regex pattern.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "File or directory to search in" },
                    "glob": { "type": "string", "description": "Glob filter for file names" }
                },
                "required": ["pattern"]
            }),
        });
    }

    if grant.contains(ToolGrant::WRITE) {
        tools.push(FlickToolDef {
            name: "write_file".into(),
            description: "Write content to a file, creating it if necessary.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to write to" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
        });
        tools.push(FlickToolDef {
            name: "edit_file".into(),
            description: "Replace a specific string in a file with new content.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to edit" },
                    "old_string": { "type": "string", "description": "Exact text to find and replace" },
                    "new_string": { "type": "string", "description": "Replacement text" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        });
    }

    if grant.contains(ToolGrant::BASH) {
        tools.push(FlickToolDef {
            name: "bash".into(),
            description: "Execute a bash command and return its output.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to execute" },
                    "timeout": { "type": "integer", "description": "Timeout in seconds (default 120)" }
                },
                "required": ["command"]
            }),
        });
    }

    tools
}

/// Result of executing a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Execute a tool call. Currently stubbed — all calls return an error indicating
/// tool execution is not yet implemented.
pub fn execute_tool(
    tool_use_id: String,
    _name: &str,
    _input: &JsonValue,
    _project_root: &Path,
    _grant: ToolGrant,
) -> ToolExecResult {
    ToolExecResult {
        tool_use_id,
        content: "tool execution not yet implemented".into(),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_gets_read_only() {
        let grant = phase_tools(AgentMethod::Analyze);
        assert!(grant.contains(ToolGrant::READ));
        assert!(!grant.contains(ToolGrant::WRITE));
        assert!(!grant.contains(ToolGrant::BASH));
    }

    #[test]
    fn execute_gets_read_write_bash() {
        let grant = phase_tools(AgentMethod::Execute);
        assert!(grant.contains(ToolGrant::READ));
        assert!(grant.contains(ToolGrant::WRITE));
        assert!(grant.contains(ToolGrant::BASH));
    }

    #[test]
    fn read_only_tools() {
        let tools = tool_definitions(ToolGrant::READ);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"bash"));
    }

    #[test]
    fn full_tools() {
        let grant = ToolGrant::READ | ToolGrant::WRITE | ToolGrant::BASH;
        let tools = tool_definitions(grant);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"bash"));
    }

    #[test]
    fn empty_grant_no_tools() {
        let tools = tool_definitions(ToolGrant::empty());
        assert!(tools.is_empty());
    }

    #[test]
    fn execute_tool_stub() {
        let result = execute_tool("tu_42".into(), "read_file", &serde_json::json!({}), Path::new("/"), ToolGrant::READ);
        assert!(result.is_error);
        assert!(result.content.contains("not yet implemented"));
        assert_eq!(result.tool_use_id, "tu_42");
    }
}
