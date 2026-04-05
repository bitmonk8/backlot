use serde::Deserialize;
use std::path::Path;

use crate::error::ConfigError;
use crate::model::ReasoningLevel;
use crate::provider::{ToolChoice, ToolDefinition};

/// Controls whether and how long cache breakpoints persist.
///
/// Injected at two positions in Messages API requests: system prompt block
/// and last user message block. Other providers ignore these annotations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheRetention {
    /// No `cache_control` injected.
    None,
    /// Standard 5-minute ephemeral TTL.
    #[default]
    Short,
    /// Extended 1-hour TTL (2x base input write cost). Only effective on Messages API.
    Long,
}

/// Config string format for `RequestConfig::from_str`.
#[derive(Debug, Clone, Copy)]
pub enum ConfigFormat {
    Yaml,
    Json,
}

/// Per-invocation request configuration.
///
/// The `model` field is a string key into the `ModelRegistry`. Provider and
/// model identity are resolved at `FlickClient` construction time, not here.
///
/// `temperature` and `reasoning` live here (not in `ModelInfo`) because the
/// same model may be called with different settings depending on the task.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestConfig {
    model: String,

    #[serde(default)]
    system_prompt: Option<String>,

    #[serde(default)]
    temperature: Option<f32>,

    #[serde(default)]
    reasoning: Option<ReasoningConfig>,

    #[serde(default)]
    output_schema: Option<OutputSchema>,

    #[serde(default)]
    tool_choice: Option<ToolChoiceConfig>,

    #[serde(default)]
    tools: Vec<ToolConfig>,

    /// Not deserialized from config files — set programmatically via builder or setter.
    #[serde(skip)]
    cache_retention: CacheRetention,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningConfig {
    pub level: ReasoningLevel,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSchema {
    pub schema: serde_json::Value,
}

/// Tool selection strategy from config.
///
/// Valid values for `type`: `"auto"`, `"any"`, `"none"`, `"tool"`.
/// When `type` is `"tool"`, `name` is required.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolChoiceConfig {
    #[serde(rename = "type")]
    pub choice_type: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// A tool definition from the tools list in config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    name: String,
    description: String,
    #[serde(default, alias = "parameters")]
    input_schema: Option<serde_json::Value>,
}

impl ToolChoiceConfig {
    /// Convert to the provider's `ToolChoice` enum.
    pub fn to_tool_choice(&self) -> ToolChoice {
        match self.choice_type.as_str() {
            "any" => ToolChoice::Any,
            "none" => ToolChoice::None,
            "tool" => ToolChoice::Tool(self.name.clone().unwrap_or_default()),
            // "auto" and any unrecognized type (rejected by validate_local)
            _ => ToolChoice::Auto,
        }
    }
}

impl ToolConfig {
    /// Construct a tool config programmatically.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Option<serde_json::Value>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub const fn input_schema(&self) -> Option<&serde_json::Value> {
        self.input_schema.as_ref()
    }

    /// Convert to the provider's `ToolDefinition` type.
    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

impl RequestConfig {
    pub async fn load(path: &Path) -> Result<Self, ConfigError> {
        let ext = path.extension().and_then(|e| e.to_str());
        match ext {
            Some("yaml" | "yml" | "json") => {}
            _ => {
                return Err(ConfigError::UnsupportedFormat(path.display().to_string()));
            }
        }
        let text = tokio::fs::read_to_string(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound(path.to_path_buf())
            } else {
                ConfigError::from(e)
            }
        })?;
        let format = if ext == Some("json") {
            ConfigFormat::Json
        } else {
            ConfigFormat::Yaml
        };
        Self::from_str(&text, format)
    }

    /// Parse and validate a YAML config string.
    pub fn parse_yaml(s: &str) -> Result<Self, ConfigError> {
        Self::from_str(s, ConfigFormat::Yaml)
    }

    /// Parse from a string with an explicit format. No file I/O.
    ///
    /// Validates tool definitions. Model/provider resolution is deferred to
    /// `FlickClient::new()`.
    pub fn from_str(s: &str, format: ConfigFormat) -> Result<Self, ConfigError> {
        let config: Self = match format {
            ConfigFormat::Yaml => {
                serde_yml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?
            }
            ConfigFormat::Json => {
                serde_json::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?
            }
        };
        config.validate_local()?;
        Ok(config)
    }

    /// Builder entrypoint.
    pub fn builder() -> RequestConfigBuilder {
        RequestConfigBuilder::default()
    }

    // -- Getters --

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub const fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    pub const fn reasoning(&self) -> Option<&ReasoningConfig> {
        self.reasoning.as_ref()
    }

    pub const fn output_schema(&self) -> Option<&OutputSchema> {
        self.output_schema.as_ref()
    }

    pub const fn tool_choice(&self) -> Option<&ToolChoiceConfig> {
        self.tool_choice.as_ref()
    }

    pub fn tools(&self) -> &[ToolConfig] {
        &self.tools
    }

    pub const fn cache_retention(&self) -> CacheRetention {
        self.cache_retention
    }

    pub const fn set_cache_retention(&mut self, retention: CacheRetention) {
        self.cache_retention = retention;
    }

    /// Append tool definitions post-parse. Validates the combined set.
    pub fn add_tools(&mut self, tools: Vec<ToolConfig>) -> Result<(), ConfigError> {
        self.tools.extend(tools);
        self.validate_local()
    }

    /// Validate fields that don't require registry lookups.
    fn validate_local(&self) -> Result<(), ConfigError> {
        if self.model.is_empty() {
            return Err(ConfigError::InvalidModelConfig(
                "model key cannot be empty".into(),
            ));
        }

        if let Some(temp) = self.temperature {
            if !temp.is_finite() || temp < 0.0 {
                return Err(ConfigError::InvalidModelConfig(
                    "temperature must be non-negative and finite".into(),
                ));
            }
        }

        // Validate tool_choice
        if let Some(tc) = &self.tool_choice {
            match tc.choice_type.as_str() {
                "auto" | "any" | "none" => {
                    if tc.name.is_some() {
                        return Err(ConfigError::InvalidModelConfig(format!(
                            "tool_choice type '{}' does not accept a name",
                            tc.choice_type
                        )));
                    }
                }
                "tool" => {
                    if tc.name.as_ref().is_none_or(String::is_empty) {
                        return Err(ConfigError::InvalidModelConfig(
                            "tool_choice type 'tool' requires a non-empty name".into(),
                        ));
                    }
                }
                other => {
                    return Err(ConfigError::InvalidModelConfig(format!(
                        "tool_choice type must be auto, any, none, or tool — got '{other}'"
                    )));
                }
            }
        }

        // Validate tools
        let mut seen_names = std::collections::HashSet::new();
        for tool in &self.tools {
            if tool.name.is_empty() {
                return Err(ConfigError::InvalidToolConfig(
                    "tool name cannot be empty".into(),
                ));
            }
            if tool.description.is_empty() {
                return Err(ConfigError::InvalidToolConfig(format!(
                    "tool '{}': description cannot be empty",
                    tool.name
                )));
            }
            if let Some(schema) = &tool.input_schema {
                if !schema.is_object() {
                    return Err(ConfigError::InvalidToolConfig(format!(
                        "tool '{}': input_schema must be a JSON object",
                        tool.name
                    )));
                }
            }
            if !seen_names.insert(&tool.name) {
                return Err(ConfigError::InvalidToolConfig(format!(
                    "tool '{}': duplicate tool name",
                    tool.name
                )));
            }
        }

        Ok(())
    }
}

/// Builder for `RequestConfig`.
#[derive(Default)]
pub struct RequestConfigBuilder {
    model: Option<String>,
    system_prompt: Option<String>,
    temperature: Option<f32>,
    reasoning: Option<ReasoningConfig>,
    output_schema: Option<OutputSchema>,
    tool_choice: Option<ToolChoiceConfig>,
    tools: Vec<ToolConfig>,
    cache_retention: CacheRetention,
}

impl RequestConfigBuilder {
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    #[must_use]
    pub const fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    #[must_use]
    pub const fn reasoning(mut self, level: ReasoningLevel) -> Self {
        self.reasoning = Some(ReasoningConfig { level });
        self
    }

    #[must_use]
    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(OutputSchema { schema });
        self
    }

    #[must_use]
    pub fn tool_choice(mut self, choice: ToolChoiceConfig) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    #[must_use]
    pub fn tools(mut self, tools: Vec<ToolConfig>) -> Self {
        self.tools = tools;
        self
    }

    #[must_use]
    pub const fn cache_retention(mut self, retention: CacheRetention) -> Self {
        self.cache_retention = retention;
        self
    }

    pub fn build(self) -> Result<RequestConfig, ConfigError> {
        let config = RequestConfig {
            model: self
                .model
                .ok_or_else(|| ConfigError::InvalidModelConfig("model is required".into()))?,
            system_prompt: self.system_prompt,
            temperature: self.temperature,
            reasoning: self.reasoning,
            output_schema: self.output_schema,
            tool_choice: self.tool_choice,
            tools: self.tools,
            cache_retention: self.cache_retention,
        };
        config.validate_local()?;
        Ok(config)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_config_ext(content: &str, ext: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(ext)
            .tempfile()
            .expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        f
    }

    fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
        write_temp_config_ext(content, ".yaml")
    }

    #[tokio::test]
    async fn load_valid_minimal_config() {
        let yaml = "model: balanced\n";
        let f = write_temp_config(yaml);
        let config = RequestConfig::load(f.path()).await.expect("should parse");
        assert_eq!(config.model(), "balanced");
    }

    #[tokio::test]
    async fn load_not_found_returns_not_found() {
        let result = RequestConfig::load(Path::new("/nonexistent/path/config.yaml")).await;
        assert!(matches!(result, Err(ConfigError::NotFound(_))));
    }

    #[tokio::test]
    async fn load_invalid_yaml_returns_parse_error() {
        let f = write_temp_config("{{{{not valid yaml");
        let result = RequestConfig::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[tokio::test]
    async fn load_unsupported_extension_rejected() {
        let mut f = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("create temp file");
        f.write_all(b"dummy").expect("write");
        let result = RequestConfig::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnsupportedFormat(_))));
    }

    #[tokio::test]
    async fn load_json_config() {
        let json = r#"{ "model": "balanced" }"#;
        let f = write_temp_config_ext(json, ".json");
        let config = RequestConfig::load(f.path())
            .await
            .expect("should parse JSON");
        assert_eq!(config.model(), "balanced");
    }

    #[tokio::test]
    async fn load_yml_extension() {
        let yaml = "model: balanced\n";
        let f = write_temp_config_ext(yaml, ".yml");
        let config = RequestConfig::load(f.path())
            .await
            .expect("should parse .yml");
        assert_eq!(config.model(), "balanced");
    }

    #[test]
    fn validate_empty_model_rejected() {
        let yaml = "model: \"\"\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(_))));
    }

    #[test]
    fn validate_negative_temperature_rejected() {
        let yaml = "model: test\ntemperature: -0.5\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("non-negative"))
        );
    }

    #[test]
    fn tools_empty_name_rejected() {
        let yaml = "model: test\ntools:\n  - name: \"\"\n    description: empty name\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("empty"))
        );
    }

    #[test]
    fn tools_duplicate_name_rejected() {
        let yaml = "model: test\ntools:\n  - name: my_tool\n    description: first\n  - name: my_tool\n    description: duplicate\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("duplicate"))
        );
    }

    #[test]
    fn tools_empty_description_rejected() {
        let yaml = "model: test\ntools:\n  - name: my_tool\n    description: \"\"\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("description"))
        );
    }

    #[test]
    fn tools_parameters_string_rejected() {
        let yaml = "model: test\ntools:\n  - name: my_tool\n    description: a tool\n    parameters: \"not an object\"\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("input_schema") && msg.contains("object"))
        );
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        let yaml = "model: test\nextra_field: oops\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(matches!(result, Err(ConfigError::Parse(msg)) if msg.contains("extra_field")));
    }

    #[test]
    fn deserialize_reasoning_config() {
        let yaml = "model: test\nreasoning:\n  level: high\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let reasoning = config.reasoning().expect("reasoning should be Some");
        assert!(matches!(
            reasoning.level,
            crate::model::ReasoningLevel::High
        ));
    }

    #[test]
    fn deserialize_output_schema() {
        let yaml = "model: test\noutput_schema:\n  schema:\n    type: object\n    properties:\n      answer:\n        type: string\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let schema = config
            .output_schema()
            .expect("output_schema should be Some");
        assert_eq!(schema.schema["type"], "object");
    }

    #[test]
    fn tools_array_parsing() {
        let yaml = r#"
model: test
tools:
  - name: read_file
    description: "Read a file's contents"
    parameters:
      type: object
      properties:
        path:
          type: string
      required:
        - path
  - name: grep_project
    description: Search for a pattern
"#;
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        assert_eq!(config.tools().len(), 2);
        assert_eq!(config.tools()[0].name(), "read_file");
        assert!(config.tools()[0].input_schema().is_some());
        assert_eq!(config.tools()[1].name(), "grep_project");
        assert!(config.tools()[1].input_schema().is_none());
    }

    #[test]
    fn builder_basic() {
        let config = RequestConfig::builder()
            .model("fast")
            .system_prompt("Be helpful")
            .temperature(0.5)
            .build()
            .expect("build");
        assert_eq!(config.model(), "fast");
        assert_eq!(config.system_prompt(), Some("Be helpful"));
        assert_eq!(config.temperature(), Some(0.5));
    }

    #[test]
    fn builder_missing_model_fails() {
        let result = RequestConfig::builder().build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_with_reasoning() {
        let config = RequestConfig::builder()
            .model("strong")
            .reasoning(crate::model::ReasoningLevel::High)
            .build()
            .expect("build");
        assert!(config.reasoning().is_some());
    }

    #[test]
    fn cross_format_yaml_json_equivalence() {
        let yaml = r#"
model: test
system_prompt: "Be helpful"
tools:
  - name: read_file
    description: "Read a file"
"#;
        let json = r#"{
            "model": "test",
            "system_prompt": "Be helpful",
            "tools": [{ "name": "read_file", "description": "Read a file" }]
        }"#;
        let from_yaml = RequestConfig::parse_yaml(yaml).expect("yaml");
        let from_json = RequestConfig::from_str(json, ConfigFormat::Json).expect("json");

        assert_eq!(from_yaml.model(), from_json.model());
        assert_eq!(from_yaml.system_prompt(), from_json.system_prompt());
        assert_eq!(from_yaml.tools().len(), from_json.tools().len());
    }

    #[test]
    fn tool_config_to_definition() {
        let tool = ToolConfig {
            name: "test_tool".into(),
            description: "A test tool".into(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "arg": {"type": "string"} },
                "required": ["arg"]
            })),
        };
        let def = tool.to_definition();
        assert_eq!(def.name, "test_tool");
        assert_eq!(def.description, "A test tool");
        assert!(def.input_schema.is_some());
    }

    #[test]
    fn valid_yaml_wrong_schema_returns_parse_error() {
        let result = RequestConfig::parse_yaml("model_wrong: \"a string\"");
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn empty_file_returns_parse_error() {
        let result = RequestConfig::parse_yaml("");
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[tokio::test]
    async fn no_file_extension_returns_unsupported_format() {
        let mut f = tempfile::Builder::new()
            .prefix("config")
            .suffix("")
            .tempfile()
            .expect("create temp file");
        f.write_all(b"dummy").expect("write");
        let result = RequestConfig::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnsupportedFormat(_))));
    }

    #[test]
    fn unknown_tool_field_rejected() {
        let yaml =
            "model: test\ntools:\n  - name: my_tool\n    description: a tool\n    timeout: 30\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(matches!(result, Err(ConfigError::Parse(msg)) if msg.contains("timeout")));
    }

    #[test]
    fn tool_choice_auto() {
        let yaml = "model: test\ntool_choice:\n  type: auto\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let tc = config.tool_choice().expect("tool_choice should be Some");
        assert_eq!(tc.choice_type, "auto");
        assert!(tc.name.is_none());
        let resolved = tc.to_tool_choice();
        assert_eq!(resolved, crate::provider::ToolChoice::Auto);
    }

    #[test]
    fn tool_choice_any() {
        let yaml = "model: test\ntool_choice:\n  type: any\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let tc = config.tool_choice().expect("tool_choice should be Some");
        let resolved = tc.to_tool_choice();
        assert_eq!(resolved, crate::provider::ToolChoice::Any);
    }

    #[test]
    fn tool_choice_none_value() {
        let yaml = "model: test\ntool_choice:\n  type: none\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let tc = config.tool_choice().expect("tool_choice should be Some");
        let resolved = tc.to_tool_choice();
        assert_eq!(resolved, crate::provider::ToolChoice::None);
    }

    #[test]
    fn tool_choice_specific_tool() {
        let yaml = "model: test\ntool_choice:\n  type: tool\n  name: read_file\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let tc = config.tool_choice().expect("tool_choice should be Some");
        let resolved = tc.to_tool_choice();
        assert_eq!(
            resolved,
            crate::provider::ToolChoice::Tool("read_file".into())
        );
    }

    #[test]
    fn tool_choice_tool_without_name_rejected() {
        let yaml = "model: test\ntool_choice:\n  type: tool\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("name"))
        );
    }

    #[test]
    fn tool_choice_invalid_type_rejected() {
        let yaml = "model: test\ntool_choice:\n  type: foo\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("foo")));
    }

    #[test]
    fn tool_choice_auto_with_name_rejected() {
        let yaml = "model: test\ntool_choice:\n  type: auto\n  name: read_file\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("does not accept"))
        );
    }

    #[test]
    fn tool_choice_any_with_name_rejected() {
        let yaml = "model: test\ntool_choice:\n  type: any\n  name: read_file\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("does not accept"))
        );
    }

    #[test]
    fn tool_choice_none_with_name_rejected() {
        let yaml = "model: test\ntool_choice:\n  type: none\n  name: read_file\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("does not accept"))
        );
    }

    #[test]
    fn tool_choice_tool_with_empty_name_rejected() {
        let yaml = "model: test\ntool_choice:\n  type: tool\n  name: \"\"\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("name"))
        );
    }
}
