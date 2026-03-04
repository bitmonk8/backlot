use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::ApiKind;
use crate::error::ConfigError;
use crate::model::{anthropic_budget_tokens, ReasoningLevel};
use crate::provider::ToolDefinition;

/// Top-level configuration.
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

/// A tool definition from the tools list in config.
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

#[derive(Debug, Deserialize)]
pub struct PricingConfig {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self, ConfigError> {
        let ext = path.extension().and_then(|e| e.to_str());
        // Validate extension before reading the file.
        match ext {
            Some("yaml" | "yml" | "json") => {}
            Some(_) => {
                return Err(ConfigError::UnsupportedFormat(
                    path.display().to_string(),
                ));
            }
            None => {
                return Err(ConfigError::UnsupportedFormat(
                    path.display().to_string(),
                ));
            }
        }
        let text = tokio::fs::read_to_string(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound(path.to_path_buf())
            } else {
                ConfigError::from(e)
            }
        })?;
        match ext {
            Some("json") => {
                let config: Self =
                    serde_json::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
                config.validate()?;
                Ok(config)
            }
            _ => {
                let config: Self =
                    serde_yml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
                config.validate()?;
                Ok(config)
            }
        }
    }

    /// Parse and validate a YAML config string.
    pub fn parse_yaml(s: &str) -> Result<Self, ConfigError> {
        let config: Self = serde_yml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
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
        let yaml = r"
model:
  provider: anthropic
  name: claude-sonnet-4-20250514

provider:
  anthropic:
    api: messages
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.model.name, "claude-sonnet-4-20250514");
        assert!(config.model.max_tokens.is_none());
    }

    #[tokio::test]
    async fn load_not_found_returns_not_found() {
        let result = Config::load(Path::new("/nonexistent/path/config.yaml")).await;
        assert!(matches!(result, Err(ConfigError::NotFound(_))));
    }

    #[tokio::test]
    async fn load_invalid_yaml_returns_parse_error() {
        let f = write_temp_config("{{{{not valid yaml");
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[tokio::test]
    async fn load_unsupported_extension_rejected() {
        let mut f = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("create temp file");
        f.write_all(b"dummy").expect("write");
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnsupportedFormat(_))));
    }

    #[tokio::test]
    async fn load_json_config() {
        let json = r#"{
            "model": { "provider": "anthropic", "name": "claude-sonnet-4-20250514" },
            "provider": { "anthropic": { "api": "messages" } }
        }"#;
        let f = write_temp_config_ext(json, ".json");
        let config = Config::load(f.path()).await.expect("should parse JSON");
        assert_eq!(config.model.name, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn load_yml_extension() {
        let yaml = "model:\n  provider: anthropic\n  name: claude-sonnet-4-20250514\nprovider:\n  anthropic:\n    api: messages\n";
        let f = write_temp_config_ext(yaml, ".yml");
        let config = Config::load(f.path()).await.expect("should parse .yml");
        assert_eq!(config.model.name, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn load_invalid_json_returns_parse_error() {
        let f = write_temp_config_ext("{not valid json", ".json");
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[tokio::test]
    async fn active_provider_valid() {
        let yaml = r"
model:
  provider: anthropic
  name: test-model

provider:
  anthropic:
    api: messages
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let p = config.active_provider();
        p.expect("should resolve");
    }

    #[tokio::test]
    async fn active_provider_missing_with_providers_defined() {
        let yaml = r"
model:
  provider: nonexistent
  name: test-model

provider:
  anthropic:
    api: messages
";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[tokio::test]
    async fn validate_provider_reference_no_providers_defined() {
        let yaml = r"
model:
  provider: anthropic
  name: test-model
";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[tokio::test]
    async fn pricing_from_builtin_registry() {
        let yaml = r"
model:
  provider: test
  name: claude-sonnet-4-20250514

provider:
  test:
    api: messages
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let (inp, out) = config.pricing();
        assert!(inp > 0.0);
        assert!(out > 0.0);
    }

    #[tokio::test]
    async fn pricing_unknown_model_returns_zero() {
        let yaml = r"
model:
  provider: test
  name: totally-unknown-model-xyz

provider:
  test:
    api: messages
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let (inp, out) = config.pricing();
        assert!((inp - 0.0).abs() < f64::EPSILON);
        assert!((out - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn compute_cost_basic() {
        let yaml = r"
model:
  provider: test
  name: unknown-model

provider:
  test:
    api: messages

pricing:
  input_per_million: 3.0
  output_per_million: 15.0
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let cost = config.compute_cost(1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn deserialize_reasoning_config() {
        let yaml = r"
model:
  provider: test
  name: test-model
  max_tokens: 64000
  reasoning:
    level: high

provider:
  test:
    api: messages
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let reasoning = config.model.reasoning.expect("reasoning should be Some");
        assert!(matches!(reasoning.level, crate::model::ReasoningLevel::High));
    }

    #[tokio::test]
    async fn deserialize_output_schema() {
        let yaml = r"
model:
  provider: test
  name: test-model

provider:
  test:
    api: messages

output_schema:
  schema:
    type: object
    properties:
      answer:
        type: string
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let schema = config.output_schema.expect("output_schema should be Some");
        assert_eq!(schema.schema["type"], "object");
    }

    #[tokio::test]
    async fn deserialize_provider_compat_flags() {
        let yaml = r"
model:
  provider: test
  name: test-model

provider:
  test:
    api: chat_completions
    compat:
      explicit_tool_choice_auto: true
";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let provider = config.active_provider().expect("provider should resolve");
        let compat = provider.compat.as_ref().expect("compat should be Some");
        assert!(compat.explicit_tool_choice_auto);
    }

    #[tokio::test]
    async fn validate_empty_provider_name() {
        let yaml = "model:\n  provider: \"\"\n  name: test-model\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[tokio::test]
    async fn validate_max_tokens_zero_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  max_tokens: 0\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("max_tokens")));
    }

    #[tokio::test]
    async fn max_tokens_none_when_omitted() {
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert!(config.model().max_tokens().is_none());
    }

    #[tokio::test]
    async fn max_tokens_some_when_set() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  max_tokens: 4096\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.model().max_tokens(), Some(4096));
    }

    #[tokio::test]
    async fn validate_temperature_out_of_range_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: 2.5\nprovider:\n  test:\n    api: chat_completions\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("temperature")));
    }

    #[tokio::test]
    async fn validate_temperature_zero_accepted() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: 0.0\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_temperature_two_accepted_openai() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: 2.0\nprovider:\n  test:\n    api: chat_completions\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_temperature_one_accepted_anthropic() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: 1.0\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_temperature_above_one_rejected_anthropic() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: 1.5\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("temperature")));
    }

    #[tokio::test]
    async fn validate_temperature_above_one_accepted_openai() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: 1.5\nprovider:\n  test:\n    api: chat_completions\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_negative_input_pricing_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\npricing:\n  input_per_million: -1.0\n  output_per_million: 5.0\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("input_per_million")));
    }

    #[tokio::test]
    async fn validate_negative_output_pricing_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\npricing:\n  input_per_million: 3.0\n  output_per_million: -2.0\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("output_per_million")));
    }

    #[tokio::test]
    async fn explicit_pricing_overrides_builtin() {
        let yaml = "model:\n  provider: test\n  name: claude-sonnet-4-20250514\nprovider:\n  test:\n    api: messages\npricing:\n  input_per_million: 99.0\n  output_per_million: 199.0\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let (inp, out) = config.pricing();
        assert!((inp - 99.0).abs() < f64::EPSILON);
        assert!((out - 199.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn compute_cost_zero_tokens() {
        let yaml = "model:\n  provider: test\n  name: unknown-model\nprovider:\n  test:\n    api: messages\npricing:\n  input_per_million: 3.0\n  output_per_million: 15.0\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        let cost = config.compute_cost(0, 0);
        assert!((cost - 0.0).abs() < 1e-15);
    }

    #[tokio::test]
    async fn validate_negative_temperature_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  temperature: -0.5\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("non-negative")));
    }

    #[tokio::test]
    async fn validate_budget_tokens_exceeds_max_tokens_rejected() {
        let yaml = "model:\n  provider: test\n  name: claude-sonnet-4-20250514\n  max_tokens: 1024\n  reasoning:\n    level: high\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        // High = 32000, max_tokens = 1024 -> rejected
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("budget_tokens")));
    }

    #[tokio::test]
    async fn validate_budget_tokens_within_max_tokens_accepted() {
        let yaml = "model:\n  provider: test\n  name: claude-sonnet-4-20250514\n  max_tokens: 64000\n  reasoning:\n    level: high\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_budget_tokens_not_checked_for_chat_completions() {
        let yaml = "model:\n  provider: test\n  name: o3-mini\n  reasoning:\n    level: high\nprovider:\n  test:\n    api: chat_completions\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn override_model_name_success() {
        let yaml = "model:\n  provider: test\n  name: original-model\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.model().name(), "original-model");
        config.override_model_name("new-model".into()).expect("override should succeed");
        assert_eq!(config.model().name(), "new-model");
    }

    #[tokio::test]
    async fn override_model_name_reverts_on_failure() {
        let yaml = "model:\n  provider: test\n  name: original-model\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        let result = config.override_model_name(String::new());
        assert!(result.is_err());
        assert_eq!(config.model().name(), "original-model", "should revert to original");
    }

    #[tokio::test]
    async fn override_reasoning_success() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  max_tokens: 64000\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        assert!(config.model().reasoning().is_none());
        config.override_reasoning(super::ReasoningConfig { level: crate::model::ReasoningLevel::Medium }).expect("override should succeed");
        assert!(config.model().reasoning().is_some());
    }

    #[tokio::test]
    async fn override_reasoning_reverts_on_failure() {
        let yaml = "model:\n  provider: test\n  name: test-model\n  max_tokens: 1024\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let mut config = Config::load(f.path()).await.expect("should parse");
        assert!(config.model().reasoning().is_none());
        let result = config.override_reasoning(super::ReasoningConfig { level: crate::model::ReasoningLevel::High });
        assert!(result.is_err());
        assert!(config.model().reasoning().is_none(), "should revert to None");
    }

    #[tokio::test]
    async fn unknown_fields_in_provider_section_ignored() {
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\n    base_url: https://stale.example.com\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse despite unknown base_url field");
        assert_eq!(config.active_provider().expect("provider").api, ApiKind::Messages);
    }

    // -- tools list tests --

    #[tokio::test]
    async fn tools_array_parsing() {
        let yaml = r#"
model:
  provider: test
  name: test-model

provider:
  test:
    api: messages

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
        let f = write_temp_config(yaml);
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
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\n";
        let f = write_temp_config(yaml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert!(config.tools().is_empty());
    }

    #[tokio::test]
    async fn tools_empty_name_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\ntools:\n  - name: \"\"\n    description: empty name\n";
        let f = write_temp_config(yaml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("empty")));
    }

    #[tokio::test]
    async fn tools_duplicate_name_rejected() {
        let yaml = "model:\n  provider: test\n  name: test-model\nprovider:\n  test:\n    api: messages\ntools:\n  - name: my_tool\n    description: first\n  - name: my_tool\n    description: duplicate\n";
        let f = write_temp_config(yaml);
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

    #[test]
    fn cross_format_yaml_json_equivalence() {
        let yaml = r#"
model:
  provider: test
  name: test-model
  max_tokens: 2048

system_prompt: "Be helpful"

provider:
  test:
    api: messages

tools:
  - name: read_file
    description: "Read a file"

pricing:
  input_per_million: 3.0
  output_per_million: 15.0
"#;
        let json = r#"{
            "model": { "provider": "test", "name": "test-model", "max_tokens": 2048 },
            "system_prompt": "Be helpful",
            "provider": { "test": { "api": "messages" } },
            "tools": [{ "name": "read_file", "description": "Read a file" }],
            "pricing": { "input_per_million": 3.0, "output_per_million": 15.0 }
        }"#;
        let from_yaml = Config::parse_yaml(yaml).expect("yaml");
        let from_json: Config =
            serde_json::from_str(json).map_err(|e| ConfigError::Parse(e.to_string())).expect("json");
        from_json.validate().expect("json validate");

        assert_eq!(from_yaml.model.name, from_json.model.name);
        assert_eq!(from_yaml.model.provider, from_json.model.provider);
        assert_eq!(from_yaml.model.max_tokens, from_json.model.max_tokens);
        assert_eq!(from_yaml.system_prompt(), from_json.system_prompt());
        assert_eq!(from_yaml.tools().len(), from_json.tools().len());
        assert_eq!(from_yaml.tools()[0].name(), from_json.tools()[0].name());
        assert_eq!(from_yaml.pricing(), from_json.pricing());
    }

    #[test]
    fn valid_yaml_wrong_schema_returns_parse_error() {
        let result = Config::parse_yaml("model: \"a string\"");
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn empty_file_returns_parse_error() {
        let result = Config::parse_yaml("");
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
        // Strip any extension by renaming — suffix("") may still have no dot
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::UnsupportedFormat(_))));
    }

    #[test]
    fn json_trailing_comma_returns_parse_error() {
        let bad_json = r#"{
            "model": { "provider": "test", "name": "m" },
            "provider": { "test": { "api": "messages" } },
        }"#;
        let result: Result<Config, _> =
            serde_json::from_str(bad_json).map_err(|e| ConfigError::Parse(e.to_string()));
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }
}
