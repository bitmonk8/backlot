use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::model::{anthropic_budget_tokens, ReasoningLevel};

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
    tools: ToolsConfig,

    #[serde(default)]
    resources: Vec<ResourceConfig>,

    #[serde(default)]
    pricing: Option<PricingConfig>,

    #[serde(default)]
    sandbox: Option<SandboxConfig>,
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
    pub base_url: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub compat: Option<CompatFlags>,
}

pub use crate::ApiKind;

/// Per-provider quirk flags. All default to false.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompatFlags {
    #[serde(default)]
    pub explicit_tool_choice_auto: bool,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub read_file: bool,
    #[serde(default)]
    pub write_file: bool,
    #[serde(default)]
    pub list_directory: bool,
    #[serde(default)]
    pub shell_exec: bool,
    #[serde(default)]
    pub custom: Vec<CustomToolConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomToolConfig {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
    /// Shell command template (uses {{param}} placeholders).
    #[serde(default)]
    pub command: Option<String>,
    /// Path to executable (receives JSON on stdin).
    #[serde(default)]
    pub executable: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceConfig {
    pub path: PathBuf,
    pub access: ResourceAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAccess {
    Read,
    ReadWrite,
}

#[derive(Debug, Deserialize)]
pub struct PricingConfig {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SandboxConfig {
    wrapper: Vec<String>,
    #[serde(default)]
    read_args: Vec<String>,
    #[serde(default)]
    read_write_args: Vec<String>,
    #[serde(default)]
    suffix: Vec<String>,
    #[serde(default)]
    policy_file: Option<String>,
    #[serde(default)]
    policy_template: Option<String>,
    #[serde(default)]
    policy_read_rule: Option<String>,
    #[serde(default)]
    policy_read_write_rule: Option<String>,
}

impl SandboxConfig {
    pub fn is_enabled(&self) -> bool {
        !self.wrapper.is_empty()
    }

    pub fn wrapper(&self) -> &[String] {
        &self.wrapper
    }

    pub fn read_args(&self) -> &[String] {
        &self.read_args
    }

    pub fn read_write_args(&self) -> &[String] {
        &self.read_write_args
    }

    pub fn suffix(&self) -> &[String] {
        &self.suffix
    }

    pub fn policy_file(&self) -> Option<&str> {
        self.policy_file.as_deref()
    }

    pub fn policy_template(&self) -> Option<&str> {
        self.policy_template.as_deref()
    }

    pub fn policy_read_rule(&self) -> Option<&str> {
        self.policy_read_rule.as_deref()
    }

    pub fn policy_read_write_rule(&self) -> Option<&str> {
        self.policy_read_write_rule.as_deref()
    }
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

    pub const fn tools(&self) -> &ToolsConfig {
        &self.tools
    }

    pub fn resources(&self) -> &[ResourceConfig] {
        &self.resources
    }

    pub const fn sandbox(&self) -> Option<&SandboxConfig> {
        self.sandbox.as_ref()
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
        // Keep in sync with ToolsConfig bool fields. The field count assertion
        // below breaks the build if a field is added/removed without updating.
        const BUILTIN_NAMES: &[&str] = &["read_file", "write_file", "list_directory", "shell_exec"];
        const _: () = assert!(BUILTIN_NAMES.len() == 4, "update BUILTIN_NAMES when ToolsConfig changes");

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
        let mut seen_names = std::collections::HashSet::new();
        for tool in &self.tools.custom {
            if tool.name.is_empty() {
                return Err(ConfigError::InvalidToolConfig(
                    "tool name cannot be empty".into(),
                ));
            }
            if BUILTIN_NAMES.contains(&tool.name.as_str()) {
                return Err(ConfigError::InvalidToolConfig(format!(
                    "tool '{}': name collides with builtin tool",
                    tool.name
                )));
            }
            if !seen_names.insert(&tool.name) {
                return Err(ConfigError::InvalidToolConfig(format!(
                    "tool '{}': duplicate custom tool name",
                    tool.name
                )));
            }
            match (&tool.command, &tool.executable) {
                (None, None) => {
                    return Err(ConfigError::InvalidToolConfig(format!(
                        "tool '{}': must have either 'command' or 'executable'",
                        tool.name
                    )));
                }
                (Some(_), Some(_)) => {
                    return Err(ConfigError::InvalidToolConfig(format!(
                        "tool '{}': cannot have both 'command' and 'executable'",
                        tool.name
                    )));
                }
                _ => {}
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

        // Validate sandbox config
        if let Some(sandbox) = &self.sandbox {
            Self::validate_sandbox(sandbox)?;
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

    fn validate_sandbox(sandbox: &SandboxConfig) -> Result<(), ConfigError> {
        if sandbox.wrapper.is_empty() {
            return Err(ConfigError::InvalidSandboxConfig(
                "wrapper must not be empty".into(),
            ));
        }
        if sandbox.wrapper[0].trim().is_empty() {
            return Err(ConfigError::InvalidSandboxConfig(
                "wrapper[0] must not be blank".into(),
            ));
        }
        match (&sandbox.policy_template, &sandbox.policy_file) {
            (Some(_), None) => {
                return Err(ConfigError::InvalidSandboxConfig(
                    "policy_template requires policy_file".into(),
                ));
            }
            (None, Some(_)) => {
                return Err(ConfigError::InvalidSandboxConfig(
                    "policy_file requires policy_template".into(),
                ));
            }
            _ => {}
        }
        if sandbox.policy_template.is_none() {
            if sandbox.policy_read_rule.is_some() {
                return Err(ConfigError::InvalidSandboxConfig(
                    "policy_read_rule requires policy_template".into(),
                ));
            }
            if sandbox.policy_read_write_rule.is_some() {
                return Err(ConfigError::InvalidSandboxConfig(
                    "policy_read_write_rule requires policy_template".into(),
                ));
            }
        }
        if sandbox.policy_file.is_none() {
            let references_policy_file = sandbox
                .wrapper
                .iter()
                .chain(sandbox.suffix.iter())
                .any(|s| s.contains("{policy_file}"));
            if references_policy_file {
                return Err(ConfigError::InvalidSandboxConfig(
                    "wrapper/suffix references {policy_file} but policy_file is not configured"
                        .into(),
                ));
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
    async fn validate_custom_tool_no_command_or_executable() {
        let toml = r#"
[model]
provider = "test"
name = "test"

[provider.test]
api = "messages"

[[tools.custom]]
name = "bad_tool"
description = "missing command and executable"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(_))));
    }

    #[tokio::test]
    async fn validate_custom_tool_both_command_and_executable() {
        let toml = r#"
[model]
provider = "test"
name = "test"

[provider.test]
api = "messages"

[[tools.custom]]
name = "bad_tool"
description = "has both"
command = "echo hello"
executable = "/usr/bin/echo"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(_))));
    }

    #[tokio::test]
    async fn validate_custom_tool_command_only_passes() {
        let toml = r#"
[model]
provider = "test"
name = "test"

[provider.test]
api = "messages"

[[tools.custom]]
name = "good_tool"
description = "has command"
command = "echo hello"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await;
        config.expect("should parse");
    }

    #[tokio::test]
    async fn validate_custom_tool_name_collision_with_builtin() {
        let toml = r#"
[model]
provider = "test"
name = "test"

[provider.test]
api = "messages"

[[tools.custom]]
name = "read_file"
description = "shadows builtin"
command = "echo hello"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(_))));
    }

    #[tokio::test]
    async fn validate_duplicate_custom_tool_names() {
        let toml = r#"
[model]
provider = "test"
name = "test"

[provider.test]
api = "messages"

[[tools.custom]]
name = "my_tool"
description = "first"
command = "echo 1"

[[tools.custom]]
name = "my_tool"
description = "duplicate"
command = "echo 2"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(_))));
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
    async fn deserialize_resources() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[[resources]]
path = "/tmp/read_only"
access = "read"

[[resources]]
path = "/tmp/full"
access = "read_write"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert_eq!(config.resources.len(), 2);
        assert_eq!(config.resources[0].access, ResourceAccess::Read);
        assert_eq!(config.resources[1].access, ResourceAccess::ReadWrite);
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
    async fn validate_empty_custom_tool_name() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[[tools.custom]]
name = ""
description = "empty name"
command = "echo hello"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidToolConfig(msg)) if msg.contains("empty")));
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
        // High = 32000, max_tokens = 1024 → rejected
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
        // OpenAI uses reasoning_effort, not budget_tokens — no validation needed
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

    // -- Sandbox config tests --

    #[tokio::test]
    async fn sandbox_minimal_wrapper_only() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap", "--die-with-parent"]
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let sandbox = config.sandbox().expect("sandbox should be Some");
        assert!(sandbox.is_enabled());
        assert_eq!(sandbox.wrapper(), &["bwrap", "--die-with-parent"]);
        assert!(sandbox.read_args().is_empty());
        assert!(sandbox.suffix().is_empty());
        assert!(sandbox.policy_file().is_none());
    }

    #[tokio::test]
    async fn sandbox_full_config() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap"]
read_args = ["--ro-bind", "{path}", "{path}"]
read_write_args = ["--bind", "{path}", "{path}"]
suffix = ["--"]
policy_file = "/tmp/flick-{pid}.sb"
policy_template = "(version 1)\n{read_rules}\n{read_write_rules}"
policy_read_rule = "(allow file-read* (subpath \"{path}\"))"
policy_read_write_rule = "(allow file-read* file-write* (subpath \"{path}\"))"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        let sandbox = config.sandbox().expect("sandbox should be Some");
        assert_eq!(sandbox.read_args(), &["--ro-bind", "{path}", "{path}"]);
        assert_eq!(sandbox.suffix(), &["--"]);
        assert!(sandbox.policy_file().is_some());
        assert!(sandbox.policy_template().is_some());
        assert!(sandbox.policy_read_rule().is_some());
        assert!(sandbox.policy_read_write_rule().is_some());
    }

    #[tokio::test]
    async fn sandbox_empty_wrapper_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = []
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("wrapper")));
    }

    #[tokio::test]
    async fn sandbox_blank_wrapper_zero_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = [""]
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("wrapper[0] must not be blank")));
    }

    #[tokio::test]
    async fn sandbox_policy_file_without_template_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap"]
policy_file = "/tmp/policy.sb"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("policy_file requires policy_template")));
    }

    #[tokio::test]
    async fn sandbox_policy_template_without_file_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap"]
policy_template = "(version 1)"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("policy_template requires policy_file")));
    }

    #[tokio::test]
    async fn sandbox_policy_read_rule_without_template_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap"]
policy_read_rule = "(allow file-read*)"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("policy_read_rule requires policy_template")));
    }

    #[tokio::test]
    async fn sandbox_policy_read_write_rule_without_template_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap"]
policy_read_write_rule = "(allow file-write*)"
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("policy_read_write_rule requires policy_template")));
    }

    #[tokio::test]
    async fn sandbox_absent_parses_normally() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"
"#;
        let f = write_temp_config(toml);
        let config = Config::load(f.path()).await.expect("should parse");
        assert!(config.sandbox().is_none());
    }

    #[tokio::test]
    async fn sandbox_wrapper_references_policy_file_without_config_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["sandbox-exec", "-f", "{policy_file}"]
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("{policy_file}") && msg.contains("not configured")));
    }

    #[tokio::test]
    async fn sandbox_suffix_references_policy_file_without_config_rejected() {
        let toml = r#"
[model]
provider = "test"
name = "test-model"

[provider.test]
api = "messages"

[sandbox]
wrapper = ["bwrap"]
suffix = ["--profile", "{policy_file}"]
"#;
        let f = write_temp_config(toml);
        let result = Config::load(f.path()).await;
        assert!(matches!(result, Err(ConfigError::InvalidSandboxConfig(msg)) if msg.contains("{policy_file}") && msg.contains("not configured")));
    }
}
