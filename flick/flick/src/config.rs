use serde::Deserialize;
use std::path::Path;

use crate::ApiKind;
use crate::error::ConfigError;
use crate::model::{ReasoningLevel, anthropic_budget_tokens};
use crate::model_registry::ModelInfo;
use crate::provider::ToolDefinition;
use crate::provider_registry::ProviderInfo;

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
#[derive(Debug, Deserialize)]
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
    tools: Vec<ToolConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningConfig {
    pub level: ReasoningLevel,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSchema {
    pub schema: serde_json::Value,
}

/// Per-provider quirk flags. All default to false.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatFlags {
    #[serde(default)]
    pub explicit_tool_choice_auto: bool,
}

/// A tool definition from the tools list in config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    name: String,
    description: String,
    #[serde(default)]
    parameters: Option<serde_json::Value>,
}

impl ToolConfig {
    /// Construct a tool config programmatically.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<serde_json::Value>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub const fn parameters(&self) -> Option<&serde_json::Value> {
        self.parameters.as_ref()
    }

    /// Convert to the provider's `ToolDefinition` type.
    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.parameters.clone(),
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

    pub fn tools(&self) -> &[ToolConfig] {
        &self.tools
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
            if let Some(params) = &tool.parameters {
                if !params.is_object() {
                    return Err(ConfigError::InvalidToolConfig(format!(
                        "tool '{}': parameters must be a JSON object",
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

    /// Full validation against resolved model/provider info.
    /// Called by `FlickClient::new()`.
    pub(crate) fn validate_resolved(
        &self,
        model_info: &ModelInfo,
        provider_info: &ProviderInfo,
    ) -> Result<(), ConfigError> {
        // Per-provider temperature ceiling
        if let Some(temp) = self.temperature {
            let max_temp = match provider_info.api {
                ApiKind::Messages => 1.0,
                ApiKind::ChatCompletions => 2.0,
            };
            if temp > max_temp {
                return Err(ConfigError::InvalidModelConfig(format!(
                    "temperature {temp} exceeds maximum {max_temp} for this provider"
                )));
            }
        }

        // Reasoning + output_schema mutual exclusion (Messages API)
        if self.reasoning.is_some()
            && self.output_schema.is_some()
            && provider_info.api == ApiKind::Messages
        {
            return Err(ConfigError::InvalidModelConfig(
                "reasoning and output_schema cannot be used together (Anthropic API limitation)"
                    .into(),
            ));
        }

        // Anthropic budget_tokens < max_tokens constraint
        if let Some(reasoning) = &self.reasoning {
            if provider_info.api == ApiKind::Messages {
                let budget = anthropic_budget_tokens(reasoning.level);
                let effective_max = model_info.max_tokens.unwrap_or(8192);
                if budget >= effective_max {
                    return Err(ConfigError::InvalidModelConfig(format!(
                        "reasoning budget_tokens ({budget}) must be less than max_tokens ({effective_max})",
                    )));
                }
            }
        }

        Ok(())
    }

    /// Compute cost in USD from token counts and model pricing.
    #[allow(clippy::cast_precision_loss)]
    pub fn compute_cost(
        &self,
        model_info: &ModelInfo,
        input_tokens: u64,
        output_tokens: u64,
    ) -> f64 {
        let inp = model_info.input_per_million.unwrap_or(0.0);
        let out = model_info.output_per_million.unwrap_or(0.0);
        (input_tokens as f64).mul_add(inp, output_tokens as f64 * out) / 1_000_000.0
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
    tools: Vec<ToolConfig>,
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
    pub fn tools(mut self, tools: Vec<ToolConfig>) -> Self {
        self.tools = tools;
        self
    }

    pub fn build(self) -> Result<RequestConfig, ConfigError> {
        let config = RequestConfig {
            model: self.model.ok_or_else(|| {
                ConfigError::InvalidModelConfig("model is required".into())
            })?,
            system_prompt: self.system_prompt,
            temperature: self.temperature,
            reasoning: self.reasoning,
            output_schema: self.output_schema,
            tools: self.tools,
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
        let config = RequestConfig::load(f.path()).await.expect("should parse JSON");
        assert_eq!(config.model(), "balanced");
    }

    #[tokio::test]
    async fn load_yml_extension() {
        let yaml = "model: balanced\n";
        let f = write_temp_config_ext(yaml, ".yml");
        let config = RequestConfig::load(f.path()).await.expect("should parse .yml");
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
            matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("parameters") && msg.contains("object"))
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
        assert!(matches!(reasoning.level, crate::model::ReasoningLevel::High));
    }

    #[test]
    fn deserialize_output_schema() {
        let yaml = "model: test\noutput_schema:\n  schema:\n    type: object\n    properties:\n      answer:\n        type: string\n";
        let config = RequestConfig::parse_yaml(yaml).expect("should parse");
        let schema = config.output_schema().expect("output_schema should be Some");
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
        assert!(config.tools()[0].parameters().is_some());
        assert_eq!(config.tools()[1].name(), "grep_project");
        assert!(config.tools()[1].parameters().is_none());
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
    fn validate_resolved_temperature_ceiling_messages() {
        let config = RequestConfig::parse_yaml("model: test\ntemperature: 1.5\n").expect("parse");
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
        };
        let provider_info = ProviderInfo {
            api: ApiKind::Messages,
            base_url: String::new(),
            key: String::new(),
            compat: None,
        };
        let result = config.validate_resolved(&model_info, &provider_info);
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("temperature")));
    }

    #[test]
    fn validate_resolved_reasoning_output_schema_rejected_messages() {
        let config = RequestConfig::parse_yaml(
            "model: test\nreasoning:\n  level: medium\noutput_schema:\n  schema:\n    type: object\n",
        )
        .expect("parse");
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: Some(64000),
            input_per_million: None,
            output_per_million: None,
        };
        let provider_info = ProviderInfo {
            api: ApiKind::Messages,
            base_url: String::new(),
            key: String::new(),
            compat: None,
        };
        let result = config.validate_resolved(&model_info, &provider_info);
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("reasoning") && msg.contains("output_schema")));
    }

    #[test]
    fn validate_resolved_budget_tokens_exceed_max() {
        let config = RequestConfig::parse_yaml(
            "model: test\nreasoning:\n  level: high\n",
        )
        .expect("parse");
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
        };
        let provider_info = ProviderInfo {
            api: ApiKind::Messages,
            base_url: String::new(),
            key: String::new(),
            compat: None,
        };
        let result = config.validate_resolved(&model_info, &provider_info);
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("budget_tokens")));
    }

    #[test]
    fn compute_cost_basic() {
        let config = RequestConfig::parse_yaml("model: test\n").expect("parse");
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: None,
            input_per_million: Some(3.0),
            output_per_million: Some(15.0),
        };
        let cost = config.compute_cost(&model_info, 1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);
    }

    #[test]
    fn compute_cost_no_pricing() {
        let config = RequestConfig::parse_yaml("model: test\n").expect("parse");
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: None,
            input_per_million: None,
            output_per_million: None,
        };
        let cost = config.compute_cost(&model_info, 1_000_000, 1_000_000);
        assert!((cost - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn tool_config_to_definition() {
        let tool = ToolConfig {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters: Some(serde_json::json!({
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
        let yaml = "model: test\ntools:\n  - name: my_tool\n    description: a tool\n    timeout: 30\n";
        let result = RequestConfig::parse_yaml(yaml);
        assert!(matches!(result, Err(ConfigError::Parse(msg)) if msg.contains("timeout")));
    }
}
