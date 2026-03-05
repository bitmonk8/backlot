use serde::{Deserialize, Serialize};

/// Abstract reasoning level, mapped per-provider to concrete parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ReasoningLevel {
    Minimal,
    Low,
    Medium,
    High,
}

/// Static info about a known model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub max_output_tokens: Option<u32>,
}

/// Look up a model by ID in the builtin registry.
///
/// Tries exact match first, then falls back to substring containment
/// (longest registry ID wins). This handles gateway-prefixed model IDs
/// like `anthropic.claude-opus-4-6-v1-engine-eng` matching `claude-opus-4-6`.
pub fn resolve_model(id: &str) -> Option<&'static ModelInfo> {
    BUILTIN_MODELS
        .iter()
        .find(|m| m.id == id)
        .or_else(|| {
            BUILTIN_MODELS
                .iter()
                .filter(|m| id.contains(m.id))
                .max_by_key(|m| m.id.len())
        })
}

/// Map reasoning level to Anthropic `budget_tokens`.
pub const fn anthropic_budget_tokens(level: ReasoningLevel) -> u32 {
    match level {
        ReasoningLevel::Minimal => 1024,
        ReasoningLevel::Low => 4096,
        ReasoningLevel::Medium => 10_000,
        ReasoningLevel::High => 32_000,
    }
}

/// Look up the default `max_output_tokens` for a model by ID.
pub fn default_max_output_tokens(model_id: &str) -> Option<u32> {
    resolve_model(model_id).and_then(|m| m.max_output_tokens)
}

/// Map reasoning level to `OpenAI` `reasoning_effort` string.
pub const fn openai_reasoning_effort(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
    }
}

static BUILTIN_MODELS: [ModelInfo; 14] = [
    // Current generation
    ModelInfo {
        id: "claude-opus-4-6",
        input_per_million: 5.0,
        output_per_million: 25.0,
        max_output_tokens: Some(128_000),
    },
    ModelInfo {
        id: "claude-sonnet-4-6",
        input_per_million: 3.0,
        output_per_million: 15.0,
        max_output_tokens: Some(64_000),
    },
    ModelInfo {
        id: "claude-haiku-4-5",
        input_per_million: 1.0,
        output_per_million: 5.0,
        max_output_tokens: Some(64_000),
    },
    // Claude 4.5
    ModelInfo {
        id: "claude-opus-4-5",
        input_per_million: 5.0,
        output_per_million: 25.0,
        max_output_tokens: Some(64_000),
    },
    ModelInfo {
        id: "claude-sonnet-4-5",
        input_per_million: 3.0,
        output_per_million: 15.0,
        max_output_tokens: Some(64_000),
    },
    // Claude 4.1
    ModelInfo {
        id: "claude-opus-4-1",
        input_per_million: 15.0,
        output_per_million: 75.0,
        max_output_tokens: Some(32_000),
    },
    // Claude 4.0
    ModelInfo {
        id: "claude-sonnet-4-20250514",
        input_per_million: 3.0,
        output_per_million: 15.0,
        max_output_tokens: Some(64_000),
    },
    ModelInfo {
        id: "claude-opus-4-20250514",
        input_per_million: 15.0,
        output_per_million: 75.0,
        max_output_tokens: Some(32_000),
    },
    // Claude 3.x
    ModelInfo {
        id: "claude-3-5-haiku-20241022",
        input_per_million: 0.80,
        output_per_million: 4.0,
        max_output_tokens: Some(8_192),
    },
    ModelInfo {
        id: "gpt-4o",
        input_per_million: 2.50,
        output_per_million: 10.0,
        max_output_tokens: Some(16_384),
    },
    ModelInfo {
        id: "gpt-4o-mini",
        input_per_million: 0.15,
        output_per_million: 0.60,
        max_output_tokens: Some(16_384),
    },
    ModelInfo {
        id: "o3-mini",
        input_per_million: 1.10,
        output_per_million: 4.40,
        max_output_tokens: Some(100_000),
    },
    ModelInfo {
        id: "deepseek-chat",
        input_per_million: 0.27,
        output_per_million: 1.10,
        max_output_tokens: Some(8_192),
    },
    ModelInfo {
        id: "deepseek-reasoner",
        input_per_million: 0.55,
        output_per_million: 2.19,
        max_output_tokens: Some(8_192),
    },
];

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_known() {
        let info = resolve_model("claude-opus-4-6").expect("known model");
        assert_eq!(info.id, "claude-opus-4-6");
        assert!((info.input_per_million - 5.0).abs() < f64::EPSILON);
        assert_eq!(info.max_output_tokens, Some(128_000));
    }

    #[test]
    fn resolve_model_unknown_returns_none() {
        assert!(resolve_model("nonexistent-model").is_none());
    }

    #[test]
    fn anthropic_budget_tokens_levels() {
        assert_eq!(anthropic_budget_tokens(ReasoningLevel::Minimal), 1024);
        assert_eq!(anthropic_budget_tokens(ReasoningLevel::Low), 4096);
        assert_eq!(anthropic_budget_tokens(ReasoningLevel::Medium), 10_000);
        assert_eq!(anthropic_budget_tokens(ReasoningLevel::High), 32_000);
    }

    #[test]
    fn openai_reasoning_effort_levels() {
        assert_eq!(openai_reasoning_effort(ReasoningLevel::Minimal), "low");
        assert_eq!(openai_reasoning_effort(ReasoningLevel::Low), "low");
        assert_eq!(openai_reasoning_effort(ReasoningLevel::Medium), "medium");
        assert_eq!(openai_reasoning_effort(ReasoningLevel::High), "high");
    }

    #[test]
    fn resolve_model_has_token_fields() {
        let info = resolve_model("claude-sonnet-4-6").expect("known model");
        assert_eq!(info.max_output_tokens, Some(64_000));
    }

    #[test]
    fn default_max_output_tokens_known_model() {
        assert_eq!(default_max_output_tokens("claude-opus-4-6"), Some(128_000));
        assert_eq!(default_max_output_tokens("claude-sonnet-4-6"), Some(64_000));
        assert_eq!(default_max_output_tokens("gpt-4o"), Some(16_384));
        assert_eq!(default_max_output_tokens("o3-mini"), Some(100_000));
    }

    #[test]
    fn default_max_output_tokens_unknown_model() {
        assert_eq!(default_max_output_tokens("nonexistent-model"), None);
    }

    #[test]
    fn resolve_model_all_entries_findable() {
        let ids = [
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-opus-4-5",
            "claude-sonnet-4-5",
            "claude-opus-4-1",
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
            "claude-3-5-haiku-20241022",
            "gpt-4o",
            "gpt-4o-mini",
            "o3-mini",
            "deepseek-chat",
            "deepseek-reasoner",
        ];
        for id in ids {
            assert!(resolve_model(id).is_some(), "missing model: {id}");
        }
    }

    #[test]
    fn resolve_model_fuzzy_gateway_prefix() {
        // Gateway-prefixed model IDs should match via substring
        let info = resolve_model("provider.claude-opus-4-6-v1-custom")
            .expect("should fuzzy-match claude-opus-4-6");
        assert_eq!(info.id, "claude-opus-4-6");
        assert_eq!(info.max_output_tokens, Some(128_000));
    }

    #[test]
    fn resolve_model_fuzzy_dated_suffix() {
        // Dated variants should match the short-form registry entry
        let cases = [
            ("provider.claude-opus-4-5-20251101-extra", "claude-opus-4-5", Some(64_000)),
            ("provider.claude-sonnet-4-5-20250929-extra", "claude-sonnet-4-5", Some(64_000)),
            ("provider.claude-opus-4-1-20250805-extra", "claude-opus-4-1", Some(32_000)),
            ("provider.claude-haiku-4-5-20251001-extra", "claude-haiku-4-5", Some(64_000)),
        ];
        for (gateway_id, expected_match, expected_tokens) in cases {
            let info = resolve_model(gateway_id)
                .unwrap_or_else(|| panic!("no match for {gateway_id}"));
            assert_eq!(info.id, expected_match, "wrong match for {gateway_id}");
            assert_eq!(info.max_output_tokens, expected_tokens, "wrong tokens for {gateway_id}");
        }
    }

    #[test]
    fn resolve_model_fuzzy_various_prefixes() {
        // Different gateway prefix styles should all resolve
        let cases = [
            ("gateway/claude-sonnet-4-6/v1", "claude-sonnet-4-6", Some(64_000)),
            ("acme.claude-opus-4-20250514-prod", "claude-opus-4-20250514", Some(32_000)),
            ("proxy.claude-3-5-haiku-20241022-v2", "claude-3-5-haiku-20241022", Some(8_192)),
        ];
        for (gateway_id, expected_match, expected_tokens) in cases {
            let info = resolve_model(gateway_id)
                .unwrap_or_else(|| panic!("no match for {gateway_id}"));
            assert_eq!(info.id, expected_match, "wrong match for {gateway_id}");
            assert_eq!(info.max_output_tokens, expected_tokens, "wrong tokens for {gateway_id}");
        }
    }

    #[test]
    fn resolve_model_fuzzy_picks_longest_match() {
        // "gpt-4o-mini" should match over "gpt-4o"
        let info = resolve_model("proxy/gpt-4o-mini/v2")
            .expect("should fuzzy-match gpt-4o-mini");
        assert_eq!(info.id, "gpt-4o-mini");
    }

    #[test]
    fn resolve_model_fuzzy_no_false_positive() {
        assert!(resolve_model("my-totally-custom-model").is_none());
    }
}
