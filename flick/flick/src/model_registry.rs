use crate::error::ConfigError;
use crate::provider_registry::flick_dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single model entry in the model registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInfo {
    /// Key into the `ProviderRegistry` — identifies which provider serves this model.
    pub provider: String,
    /// Model ID as known by the provider API (e.g. `claude-sonnet-4-6`, `gpt-4o`).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_million: Option<f64>,
}

impl ModelInfo {
    /// Compute cost in USD from token counts and model pricing.
    #[allow(clippy::cast_precision_loss, clippy::suboptimal_flops)]
    pub fn compute_cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
    ) -> f64 {
        let inp = self.input_per_million.unwrap_or(0.0);
        let out = self.output_per_million.unwrap_or(0.0);
        let cw = self.cache_creation_per_million.unwrap_or(0.0);
        let cr = self.cache_read_per_million.unwrap_or(0.0);
        (input_tokens as f64 * inp
            + output_tokens as f64 * out
            + cache_creation_input_tokens as f64 * cw
            + cache_read_input_tokens as f64 * cr)
            / 1_000_000.0
    }
}

/// Registry of named models, stored at `~/.flick/models` (TOML).
///
/// Purely user-defined — no builtin models.
pub struct ModelRegistry {
    models: BTreeMap<String, ModelInfo>,
}

impl ModelRegistry {
    /// Load from the default `~/.flick/models` file.
    pub async fn load_default() -> Result<Self, ConfigError> {
        let dir = flick_dir().map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?;
        let path = dir.join("models");
        Self::load_from_path(&path).await
    }

    /// Load from an explicit path.
    pub async fn load_from_path(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = match tokio::fs::read_to_string(path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    models: BTreeMap::new(),
                });
            }
            Err(e) => return Err(ConfigError::Io(e)),
        };
        Self::from_toml(&text)
    }

    /// Parse from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        let models: BTreeMap<String, ModelInfo> =
            toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let registry = Self { models };
        registry.validate()?;
        Ok(registry)
    }

    /// Construct an empty registry (for testing).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            models: BTreeMap::new(),
        }
    }

    /// Construct from a map (for testing).
    pub fn from_map(models: BTreeMap<String, ModelInfo>) -> Result<Self, ConfigError> {
        let registry = Self { models };
        registry.validate()?;
        Ok(registry)
    }

    pub fn get(&self, name: &str) -> Option<&ModelInfo> {
        self.models.get(name)
    }

    pub fn list(&self) -> Vec<(&str, &ModelInfo)> {
        self.models.iter().map(|(k, v)| (k.as_str(), v)).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Add or update a model entry. Validates before writing.
    pub async fn set(
        &mut self,
        key: &str,
        info: ModelInfo,
        dir: &std::path::Path,
    ) -> Result<(), ConfigError> {
        validate_model_entry(key, &info)?;
        self.models.insert(key.to_string(), info);
        self.write_to_dir(dir).await
    }

    /// Remove a model entry.
    pub async fn remove(&mut self, key: &str, dir: &std::path::Path) -> Result<bool, ConfigError> {
        let existed = self.models.remove(key).is_some();
        if existed {
            self.write_to_dir(dir).await?;
        }
        Ok(existed)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (key, info) in &self.models {
            validate_model_entry(key, info)?;
        }
        Ok(())
    }

    async fn write_to_dir(&self, dir: &std::path::Path) -> Result<(), ConfigError> {
        let text = toml::to_string(&self.models).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let path = dir.join("models");
        tokio::fs::write(&path, text).await.map_err(ConfigError::Io)
    }
}

fn validate_pricing_field(key: &str, field: &str, value: Option<f64>) -> Result<(), ConfigError> {
    if let Some(v) = value {
        if !v.is_finite() || v < 0.0 {
            return Err(ConfigError::InvalidModelConfig(format!(
                "model '{key}': {field} must be non-negative and finite"
            )));
        }
    }
    Ok(())
}

fn validate_model_entry(key: &str, info: &ModelInfo) -> Result<(), ConfigError> {
    if info.name.is_empty() {
        return Err(ConfigError::InvalidModelConfig(format!(
            "model '{key}': name cannot be empty"
        )));
    }
    if info.provider.is_empty() {
        return Err(ConfigError::InvalidModelConfig(format!(
            "model '{key}': provider cannot be empty"
        )));
    }
    if let Some(mt) = info.max_tokens {
        if mt == 0 {
            return Err(ConfigError::InvalidModelConfig(format!(
                "model '{key}': max_tokens must be greater than 0"
            )));
        }
    }
    validate_pricing_field(key, "input_per_million", info.input_per_million)?;
    validate_pricing_field(key, "output_per_million", info.output_per_million)?;
    validate_pricing_field(
        key,
        "cache_creation_per_million",
        info.cache_creation_per_million,
    )?;
    validate_pricing_field(key, "cache_read_per_million", info.cache_read_per_million)?;
    Ok(())
}

/// Check that every `ModelInfo.provider` references an existing key in the `ProviderRegistry`.
///
/// Call once after both registries are loaded, before any `FlickClient` construction.
pub async fn validate_registries(
    models: &ModelRegistry,
    providers: &crate::provider_registry::ProviderRegistry,
) -> Result<(), ConfigError> {
    let provider_list = providers
        .list()
        .await
        .map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?;
    let provider_names: std::collections::HashSet<&str> =
        provider_list.iter().map(|p| p.name.as_str()).collect();
    for (key, info) in &models.models {
        if !provider_names.contains(info.provider.as_str()) {
            return Err(ConfigError::UnknownProvider(format!(
                "model '{key}' references unknown provider '{}'",
                info.provider
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_toml() {
        let toml = r#"
[fast]
provider = "anthropic"
name = "claude-haiku-4-5-20251001"
max_tokens = 8192
input_per_million = 0.80
output_per_million = 4.00
cache_creation_per_million = 1.00
cache_read_per_million = 0.08

[balanced]
provider = "anthropic"
name = "claude-sonnet-4-6"
"#;
        let registry = ModelRegistry::from_toml(toml).expect("should parse");
        assert_eq!(registry.list().len(), 2);
        let fast = registry.get("fast").expect("fast exists");
        assert_eq!(fast.provider, "anthropic");
        assert_eq!(fast.name, "claude-haiku-4-5-20251001");
        assert_eq!(fast.max_tokens, Some(8192));
        assert_eq!(fast.cache_creation_per_million, Some(1.00));
        assert_eq!(fast.cache_read_per_million, Some(0.08));
        let balanced = registry.get("balanced").expect("balanced exists");
        assert_eq!(balanced.cache_creation_per_million, None);
        assert_eq!(balanced.cache_read_per_million, None);
    }

    #[test]
    fn empty_name_rejected() {
        let toml = r#"
[bad]
provider = "anthropic"
name = ""
"#;
        let result = ModelRegistry::from_toml(toml);
        assert!(matches!(result, Err(ConfigError::InvalidModelConfig(_))));
    }

    #[test]
    fn empty_provider_rejected() {
        let toml = r#"
[bad]
provider = ""
name = "test-model"
"#;
        let result = ModelRegistry::from_toml(toml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("provider"))
        );
    }

    #[test]
    fn zero_max_tokens_rejected() {
        let toml = r#"
[bad]
provider = "anthropic"
name = "test-model"
max_tokens = 0
"#;
        let result = ModelRegistry::from_toml(toml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("max_tokens"))
        );
    }

    #[test]
    fn negative_pricing_rejected() {
        let toml = r#"
[bad]
provider = "anthropic"
name = "test-model"
input_per_million = -1.0
"#;
        let result = ModelRegistry::from_toml(toml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("input_per_million"))
        );
    }

    #[test]
    fn negative_cache_creation_pricing_rejected() {
        let toml = r#"
[bad]
provider = "anthropic"
name = "test-model"
cache_creation_per_million = -1.0
"#;
        let result = ModelRegistry::from_toml(toml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("cache_creation_per_million"))
        );
    }

    #[test]
    fn negative_cache_read_pricing_rejected() {
        let toml = r#"
[bad]
provider = "anthropic"
name = "test-model"
cache_read_per_million = -1.0
"#;
        let result = ModelRegistry::from_toml(toml);
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("cache_read_per_million"))
        );
    }

    #[test]
    fn empty_registry_is_valid() {
        let registry = ModelRegistry::from_toml("").expect("empty is valid");
        assert!(registry.is_empty());
    }

    #[test]
    fn list_returns_sorted() {
        let toml = r#"
[zebra]
provider = "p"
name = "z-model"

[alpha]
provider = "p"
name = "a-model"
"#;
        let registry = ModelRegistry::from_toml(toml).expect("parse");
        let entries = registry.list();
        assert_eq!(entries[0].0, "alpha");
        assert_eq!(entries[1].0, "zebra");
    }

    #[test]
    fn compute_cost_basic() {
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: None,
            input_per_million: Some(3.0),
            output_per_million: Some(15.0),
            cache_creation_per_million: None,
            cache_read_per_million: None,
        };
        let cost = model_info.compute_cost(1_000_000, 1_000_000, 0, 0);
        assert!((cost - 18.0).abs() < 0.001);
    }

    #[test]
    fn compute_cost_no_pricing() {
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: None,
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        };
        let cost = model_info.compute_cost(1_000_000, 1_000_000, 0, 0);
        assert!((cost - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn compute_cost_with_cache_pricing() {
        let model_info = ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens: None,
            input_per_million: Some(3.0),
            output_per_million: Some(15.0),
            cache_creation_per_million: Some(3.75),
            cache_read_per_million: Some(0.30),
        };
        let cost = model_info.compute_cost(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        let expected = 3.0 + 15.0 + 3.75 + 0.30;
        assert!((cost - expected).abs() < 0.001);
    }

    #[tokio::test]
    async fn validate_registries_passes_when_providers_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider_registry =
            crate::provider_registry::ProviderRegistry::load(dir.path().to_path_buf());
        provider_registry
            .set(
                "my_provider",
                "key",
                crate::ApiKind::Messages,
                "https://example.com",
                None,
            )
            .await
            .expect("set provider");

        let mut models = BTreeMap::new();
        models.insert(
            "my_model".to_string(),
            ModelInfo {
                provider: "my_provider".into(),
                name: "test-model".into(),
                max_tokens: None,
                input_per_million: None,
                output_per_million: None,
                cache_creation_per_million: None,
                cache_read_per_million: None,
            },
        );
        let model_registry = ModelRegistry::from_map(models).expect("build registry");

        validate_registries(&model_registry, &provider_registry)
            .await
            .expect("validation should pass");
    }

    #[tokio::test]
    async fn validate_registries_fails_when_provider_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider_registry =
            crate::provider_registry::ProviderRegistry::load(dir.path().to_path_buf());
        // Create a provider with a different name than what the model references
        provider_registry
            .set(
                "other_provider",
                "key",
                crate::ApiKind::Messages,
                "https://example.com",
                None,
            )
            .await
            .expect("set provider");

        let mut models = BTreeMap::new();
        models.insert(
            "my_model".to_string(),
            ModelInfo {
                provider: "nonexistent_provider".into(),
                name: "test-model".into(),
                max_tokens: None,
                input_per_million: None,
                output_per_million: None,
                cache_creation_per_million: None,
                cache_read_per_million: None,
            },
        );
        let model_registry = ModelRegistry::from_map(models).expect("build registry");

        let result = validate_registries(&model_registry, &provider_registry).await;
        assert!(
            matches!(result, Err(ConfigError::UnknownProvider(msg)) if msg.contains("nonexistent_provider"))
        );
    }

    #[tokio::test]
    async fn set_and_remove() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::empty();
        registry
            .set(
                "test",
                ModelInfo {
                    provider: "p".into(),
                    name: "test-model".into(),
                    max_tokens: Some(1024),
                    input_per_million: None,
                    output_per_million: None,
                    cache_creation_per_million: None,
                    cache_read_per_million: None,
                },
                dir.path(),
            )
            .await
            .expect("set");
        assert!(registry.get("test").is_some());
        assert!(registry.remove("test", dir.path()).await.expect("remove"));
        assert!(registry.get("test").is_none());
    }
}
