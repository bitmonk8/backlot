use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::ApiKind;
use crate::error::ConfigError;
use crate::model::{anthropic_budget_tokens, ReasoningLevel};
use crate::provider::ToolDefinition;

/// Top-level TOML configuration.
///
/// Fields are private to enforce validation invariants. Use getter methods
/// for read access and `override_*` methods for CLI flag overrides.
#[derive(Debug, Deserialize)]
pub struct Config {
    model: ModelConfig,

    #[serde(default)]
    system_prompt: Option<String>,

    #[serde(default)]
    output_schema: Option<OutputSchema>,

    #[serde(default)]
    provider: HashMap<String, ProviderConfig>,

    #[serde(default)]
    tools: Vec<ToolConfig>,

    #[serde(default)]
    pricing: Option<PricingConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ModelConfig {
    provider: String,
    name: String,
    /// Maximum *output* tokens (not context window). Matches API field name.
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    reasoning: Option<ReasoningConfig>,
}

impl ModelConfig {
    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    pub const fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    pub const fn reasoning(&self) -> Option<&ReasoningConfig> {
        self.reasoning.as_ref()
    }
}

#[derive(Debug, Deserialize)]
pub struct ReasoningConfig {
    pub level: ReasoningLevel,
}

#[derive(Debug, Deserialize)]
pub struct OutputSchema {
    pub schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    pub api: ApiKind,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub compat: Option<CompatFlags>,
}

/// Per-provider quirk flags. All default to false.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompatFlags {
    #[serde(default)]
    pub explicit_tool_choice_auto: bool,
}

/// A tool definition from the `[[tools]]` array in config.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolConfig {
    name: String,
    description: String,
    #[serde(default)]
    parameters: Option<serde_json::Value>,
}

impl ToolConfig {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn parameters(&self) -> Option<&serde_json::Value> {
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

#[derive(Debug, Deserialize)]
pub struct PricingConfig {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = tokio::fs::read_to_string(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound(path.to_path_buf())
            } else {
                ConfigError::from(e)
            }
        })?;
        Self::parse(&text)
    }

    /// Parse and validate a TOML config string.
    pub fn parse(toml_str: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(toml_str)?;
        config.validate()?;
        Ok(config)
    }

    // -- Getters --

    pub const fn model(&self) -> &ModelConfig {
        &self.model
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub const fn output_schema(&self) -> Option<&OutputSchema> {
        self.output_schema.as_ref()
    }

    pub fn tools(&self) -> &[ToolConfig] {
        &self.tools
    }

    // -- CLI override methods (re-validate after mutation) --

    /// Override model name from CLI `--model` flag. Re-validates;
    /// reverts on failure.
    pub fn override_model_name(&mut self, name: String) -> Result<(), ConfigError> {
        let old = std::mem::replace(&mut self.model.name, name);
        if let Err(e) = self.validate() {
            self.model.name = old;
            return Err(e);
        }
        Ok(())
    }

    /// Override reasoning from CLI `--reasoning` flag. Re-validates
    /// `budget_tokens` constraint; reverts on failure.
    pub fn override_reasoning(
        &mut self,
        reasoning: ReasoningConfig,
    ) -> Result<(), ConfigError> {
        let old = self.model.reasoning.take();
        self.model.reasoning = Some(reasoning);
        if let Err(e) = self.validate() {
            self.model.reasoning = old;
            return Err(e);
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.model.max_tokens == Some(0) {
            return Err(ConfigError::InvalidModelConfig(
                "max_tokens must be greater than 0".into(),
            ));
        }
        if let Some(temp) = self.model.temperature {
            if !temp.is_finite() || temp < 0.0 {
                return Err(ConfigError::InvalidModelConfig(
                    "temperature must be non-negative and finite".into(),
                ));
            }
        }

        if self.model.name.is_empty() {
            return Err(ConfigError::InvalidModelConfig(
                "model name cannot be empty".into(),
            ));
        }

        if self.model.provider.is_empty() {
            return Err(ConfigError::UnknownProvider(String::new()));
        }

        // Validate tools
        let mut seen_names = std::collections::HashSet::new();
        for tool in &self.tools {
            if tool.name.is_empty() {
                return Err(ConfigError::InvalidToolConfig(
                    "tool name cannot be empty".into(),
                ));
            }
            if !seen_names.insert(&tool.name) {
                return Err(ConfigError::InvalidToolConfig(format!(
                    "tool '{}': duplicate tool name",
                    tool.name
                )));
            }
        }

        // Validate pricing
        if let Some(p) = &self.pricing {
            if !p.input_per_million.is_finite() || p.input_per_million < 0.0 {
                return Err(ConfigError::InvalidModelConfig(
                    "pricing input_per_million must be non-negative".into(),
                ));
            }
            if !p.output_per_million.is_finite() || p.output_per_million < 0.0 {
                return Err(ConfigError::InvalidModelConfig(
                    "pricing output_per_million must be non-negative".into(),
                ));
            }
        }

        // Validate provider reference
        let provider = self.active_provider()?;

        // Per-provider temperature ceiling
        if let Some(temp) = self.model.temperature {
            let max_temp = match provider.api {
                ApiKind::Messages => 1.0,
                ApiKind::ChatCompletions => 2.0,
            };
            if temp > max_temp {
                return Err(ConfigError::InvalidModelConfig(format!(
                    "temperature {temp} exceeds maximum {max_temp} for this provider"
                )));
            }
        }

        // Anthropic API requires budget_tokens < max_tokens
        if let Some(reasoning) = &self.model.reasoning {
            if provider.api == ApiKind::Messages {
                let budget = anthropic_budget_tokens(reasoning.level);
                let effective_max = self.model.max_tokens
                    .or_else(|| crate::model::default_max_output_tokens(&self.model.name))
                    .unwrap_or(8192);
                if budget >= effective_max {
                    return Err(ConfigError::InvalidModelConfig(format!(
                        "reasoning budget_tokens ({budget}) must be less than max_tokens ({effective_max})",
                    )));
                }
            }
        }

        Ok(())
    }

    /// Resolve the active provider config from the model's provider field.
    pub fn active_provider(&self) -> Result<&ProviderConfig, ConfigError> {
        let name = &self.model.provider;
        self.provider
            .get(name)
            .ok_or_else(|| ConfigError::UnknownProvider(name.clone()))
    }

    /// Get input/output pricing, preferring config overrides, then builtin
    /// model registry, then zeros.
    pub fn pricing(&self) -> (f64, f64) {
        if let Some(p) = &self.pricing {
            return (p.input_per_million, p.output_per_million);
        }
        if let Some(info) = crate::model::resolve_model(&self.model.name) {
            return (info.input_per_million, info.output_per_million);
        }
        (0.0, 0.0)
    }

    /// Compute cost in USD from token counts.
    #[allow(clippy::cast_precision_loss)]
    pub fn compute_cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        let (inp, out) = self.pricing();
        (input_tokens as f64).mul_add(inp, output_tokens as f64 * out) / 1_000_000.0
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        f
    }

    #[tokio::test]
    async fn load_valid_minimal_config() {
        let toml = r#"
[model]
provider = "anthropic"
name = "claude-sonnet-4-20250514"

[provider.anthropic]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.model.name, "claude-sonnet-4-20250514");
        assert!(config.model.max_tokens.is_none());
    }

    #[tokio::test]
    async fn load_not_found_returns_not_found() {
        let result = Config::load(Path::new("/nonexistent/path/config.toml")).await;
        assert!(matches!(result, Err(ConfigError::NotFound(_))));
    }

    #[tokio::test]
    async fn load_invalid_toml_returns_parse_error() {
        let f = write_temp_config("this is not valid toml [[[");
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[tokio::test]
    async fn active_provider_valid() {
        let toml = r#"
[model]
provider = "anthropic"
name = "test-model"

[provider.anthropic]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let p = config.active_provider();
        p.expect("should resolve");
    }

    #[tokio::test]
    async fn active_provider_missing_with_providers_defined() {
        let toml = r#"
[model]
provider = "nonexistent"
name = "test-model"

[provider.anthropic]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[tokio::test]
    async fn validate_provider_reference_no_providers_defined() {
        let toml = r#"
[model]
provider = "anthropic"
name = "test-model"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[tokio::test]
    async fn pricing_from_builtin_registry() {
        let toml = r#"
[model]
provider = "test"
name = "claude-sonnet-4-20250514"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let (inp, out) = config.pricing();
        assert!(inp > 0.0);
        assert!(out > 0.0);
    }

    #[tokio::test]
    async fn pricing_unknown_model_returns_zero() {
        let toml = r#"
[model]
provider = "test"
name = "totally-unknown-model-xyz"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let (inp, out) = config.pricing();
        assert!((inp - 0.0).abs() < f64::EPSILON);
        assert!((out - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn compute_cost_basic() {
        let toml = r#"
[model]
provider = "test"
name = "unknown-model"

[provider.test]
api = "messages"

[pricing]
input_per_million = 3.0
output_per_million = 15.0
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let cost = config.compute_cost(1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn deserialize_reasoning_config() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 64000
reasoning = {level = "high"}

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let reasoning = config.model.reasoning.expect("reasoning should be Some");
        assert!(matches!(reasoning.level, crate::model::ReasoningLevel::High));
    }

    #[tokio::test]
    async fn deserialize_output_schema() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[output_schema]
schema = {"type" = "object", "properties" = {"answer" = {"type" = "string"}}}
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let schema = config.output_schema.expect("output_schema should be Some");
        assert_eq!(schema.schema["type"], "object");
    }

    #[tokio::test]
    async fn deserialize_provider_compat_flags() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "chat_completions"

[provider.test.compat]
explicit_tool_choice_auto = true
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let provider = config.active_provider().expect("provider should resolve");
        let compat = provider.compat.as_ref().expect("compat should be Some");
        assert!(compat.explicit_tool_choice_auto);
    }

    #[tokio::test]
    async fn validate_empty_provider_name() {
        let toml = r#"
[model]
provider = ""
name = "test-model"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[tokio::test]
    async fn validate_max_tokens_zero_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 0

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("max_tokens")));
    }

    #[tokio::test]
    async fn max_tokens_none_when_omitted() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert!(config.model().max_tokens().is_none());
    }

    #[tokio::test]
    async fn max_tokens_some_when_set() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 4096

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.model().max_tokens(), Some(4096));
    }

    #[tokio::test]
    async fn validate_temperature_out_of_range_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = 2.5

[provider.test]
api = "chat_completions"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("temperature")));
    }

    #[tokio::test]
    async fn validate_temperature_zero_accepted() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = 0.0

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_temperature_two_accepted_openai() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = 2.0

[provider.test]
api = "chat_completions"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_temperature_one_accepted_anthropic() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = 1.0

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_temperature_above_one_rejected_anthropic() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = 1.5

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("temperature")));
    }

    #[tokio::test]
    async fn validate_temperature_above_one_accepted_openai() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = 1.5

[provider.test]
api = "chat_completions"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_negative_input_pricing_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[pricing]
input_per_million = -1.0
output_per_million = 5.0
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("input_per_million")));
    }

    #[tokio::test]
    async fn validate_negative_output_pricing_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[pricing]
input_per_million = 3.0
output_per_million = -2.0
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("output_per_million")));
    }

    #[tokio::test]
    async fn explicit_pricing_overrides_builtin() {
        let toml = r#"
[model]
provider = "test"
name = "claude-sonnet-4-20250514"

[provider.test]
api = "messages"

[pricing]
input_per_million = 99.0
output_per_million = 199.0
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let (inp, out) = config.pricing();
        assert!((inp - 99.0).abs() < f64::EPSILON);
        assert!((out - 199.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn compute_cost_zero_tokens() {
        let toml = r#"
[model]
provider = "test"
name = "unknown-model"

[provider.test]
api = "messages"

[pricing]
input_per_million = 3.0
output_per_million = 15.0
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let cost = config.compute_cost(0, 0);
        assert!((cost - 0.0).abs() < 1e-15);
    }

    #[tokio::test]
    async fn validate_negative_temperature_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
temperature = -0.5

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("non-negative")));
    }

    #[tokio::test]
    async fn validate_budget_tokens_exceeds_max_tokens_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "claude-sonnet-4-20250514"
max_tokens = 1024
reasoning = {level = "high"}

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        // High = 32000, max_tokens = 1024 -> rejected
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("budget_tokens")));
    }

    #[tokio::test]
    async fn validate_budget_tokens_within_max_tokens_accepted() {
        let toml = r#"
[model]
provider = "test"
name = "claude-sonnet-4-20250514"
max_tokens = 64000
reasoning = {level = "high"}

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_budget_tokens_not_checked_for_chat_completions() {
        let toml = r#"
[model]
provider = "test"
name = "o3-mini"
reasoning = {level = "high"}

[provider.test]
api = "chat_completions"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        // OpenAI uses reasoning_effort, not budget_tokens -- no validation needed
        config.expect("should parse");
    }

    #[tokio::test]
    async fn override_model_name_success() {
        let toml = r#"
[model]
provider = "test"
name = "original-model"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.model().name(), "original-model");
        config.override_model_name("new-model".into()).expect("override should succeed");
        assert_eq!(config.model().name(), "new-model");
    }

    #[tokio::test]
    async fn override_model_name_reverts_on_failure() {
        let toml = r#"
[model]
provider = "test"
name = "original-model"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        let result = config.override_model_name(String::new());
        assert!(result.is_err());
        assert_eq!(config.model().name(), "original-model", "should revert to original");
    }

    #[tokio::test]
    async fn override_reasoning_success() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 64000

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        assert!(config.model().reasoning().is_none());
        config.override_reasoning(super::ReasoningConfig { level: crate::model::ReasoningLevel::Medium }).expect("override should succeed");
        assert!(config.model().reasoning().is_some());
    }

    #[tokio::test]
    async fn override_reasoning_reverts_on_failure() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 1024

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        assert!(config.model().reasoning().is_none());
        // High level has budget_tokens=32000 which exceeds max_tokens=1024
        let result = config.override_reasoning(super::ReasoningConfig { level: crate::model::ReasoningLevel::High });
        assert!(result.is_err());
        assert!(config.model().reasoning().is_none(), "should revert to None");
    }

    #[tokio::test]
    async fn unknown_fields_in_provider_section_ignored() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"
base_url = "https://stale.example.com"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse despite unknown base_url field");
        assert_eq!(config.active_provider().expect("provider").api, ApiKind::Messages);
    }

    // -- [[tools]] array tests --

    #[tokio::test]
    async fn tools_array_parsing() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[[tools]]
name = "read_file"
description = "Read a file's contents"
parameters = { type = "object", properties = { path = { type = "string" } }, required = ["path"] }

[[tools]]
name = "grep_project"
description = "Search for a pattern"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.tools().len(), 2);
        assert_eq!(config.tools()[0].name(), "read_file");
        assert_eq!(config.tools()[0].description(), "Read a file's contents");
        assert!(config.tools()[0].parameters().is_some());
        assert_eq!(config.tools()[1].name(), "grep_project");
        assert!(config.tools()[1].parameters().is_none());
    }

    #[tokio::test]
    async fn tools_empty_array() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert!(config.tools().is_empty());
    }

    #[tokio::test]
    async fn tools_empty_name_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[[tools]]
name = ""
description = "empty name"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("empty")));
    }

    #[tokio::test]
    async fn tools_duplicate_name_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[[tools]]
name = "my_tool"
description = "first"

[[tools]]
name = "my_tool"
description = "duplicate"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("duplicate")));
    }

    #[test]
    fn tool_config_to_definition() {
        let tool = ToolConfig {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "arg": {"type": "string"}
                },
                "required": ["arg"]
            })),
        };
        let def = tool.to_definition();
        assert_eq!(def.name, "test_tool");
        assert_eq!(def.description, "A test tool");
        assert!(def.input_schema.is_some());
        assert_eq!(def.input_schema.as_ref().unwrap()["type"], "object");
    }

    #[test]
    fn tool_config_to_definition_no_parameters() {
        let tool = ToolConfig {
            name: "simple_tool".into(),
            description: "No params".into(),
            parameters: None,
        };
        let def = tool.to_definition();
        assert_eq!(def.name, "simple_tool");
        assert_eq!(def.description, "No params");
        assert!(def.input_schema.is_none());
    }
}
