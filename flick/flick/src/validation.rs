use crate::ApiKind;
use crate::config::RequestConfig;
use crate::error::ConfigError;
use crate::model::anthropic_budget_tokens;
use crate::model_registry::ModelInfo;
use crate::provider_registry::CompatFlags;
use crate::provider_registry::ProviderInfo;

/// Full validation against resolved model/provider info.
/// Called by `FlickClient::new()`.
pub fn validate_resolved(
    config: &RequestConfig,
    model_info: &ModelInfo,
    api_kind: ApiKind,
    compat: Option<&CompatFlags>,
) -> Result<(), ConfigError> {
    _ = compat; // reserved for future compat-based validation

    // Per-provider temperature ceiling
    if let Some(temp) = config.temperature() {
        let max_temp = match api_kind {
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
    if config.reasoning().is_some()
        && config.output_schema().is_some()
        && api_kind == ApiKind::Messages
    {
        return Err(ConfigError::InvalidModelConfig(
            "reasoning and output_schema cannot be used together (Anthropic API limitation)".into(),
        ));
    }

    // Anthropic budget_tokens < max_tokens constraint
    if let Some(reasoning) = config.reasoning() {
        if api_kind == ApiKind::Messages {
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

/// Adapter for `ProviderInfo` that calls the free function.
pub(crate) fn validate_resolved_from_provider_info(
    config: &RequestConfig,
    model_info: &ModelInfo,
    provider_info: &ProviderInfo,
) -> Result<(), ConfigError> {
    validate_resolved(
        config,
        model_info,
        provider_info.api,
        provider_info.compat.as_ref(),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn make_model_info(max_tokens: Option<u32>) -> ModelInfo {
        ModelInfo {
            provider: "p".into(),
            name: "m".into(),
            max_tokens,
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        }
    }

    #[test]
    fn temperature_ceiling_messages() {
        let config = RequestConfig::parse_yaml("model: test\ntemperature: 1.5\n").expect("parse");
        let result = validate_resolved(
            &config,
            &make_model_info(Some(1024)),
            ApiKind::Messages,
            None,
        );
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("temperature"))
        );
    }

    #[test]
    fn temperature_ceiling_chat_completions_ok() {
        let config = RequestConfig::parse_yaml("model: test\ntemperature: 1.5\n").expect("parse");
        let result = validate_resolved(
            &config,
            &make_model_info(Some(1024)),
            ApiKind::ChatCompletions,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn reasoning_output_schema_rejected_messages() {
        let config = RequestConfig::parse_yaml(
            "model: test\nreasoning:\n  level: medium\noutput_schema:\n  schema:\n    type: object\n",
        )
        .expect("parse");
        let result = validate_resolved(
            &config,
            &make_model_info(Some(64000)),
            ApiKind::Messages,
            None,
        );
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("reasoning") && msg.contains("output_schema"))
        );
    }

    #[test]
    fn budget_tokens_exceed_max() {
        let config =
            RequestConfig::parse_yaml("model: test\nreasoning:\n  level: high\n").expect("parse");
        let result = validate_resolved(
            &config,
            &make_model_info(Some(1024)),
            ApiKind::Messages,
            None,
        );
        assert!(
            matches!(result, Err(ConfigError::InvalidModelConfig(msg)) if msg.contains("budget_tokens"))
        );
    }
}
