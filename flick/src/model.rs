use serde::{Deserialize, Serialize};

/// Abstract reasoning level, mapped per-provider to concrete parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
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
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

/// Look up a model by ID in the builtin registry.
pub fn resolve_model(id: &str) -> Option<&'static ModelInfo> {
    BUILTIN_MODELS.iter().find(|m| m.id == id)
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

static BUILTIN_MODELS: [ModelInfo; 8] = [
    ModelInfo {
        id: "claude-sonnet-4-20250514",
        input_per_million: 3.0,
        output_per_million: 15.0,
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
    },
    ModelInfo {
        id: "claude-opus-4-20250514",
        input_per_million: 15.0,
        output_per_million: 75.0,
        context_window: Some(200_000),
        max_output_tokens: Some(32_000),
    },
    ModelInfo {
        id: "claude-haiku-3-5-20241022",
        input_per_million: 0.80,
        output_per_million: 4.0,
        context_window: Some(200_000),
        max_output_tokens: Some(8_192),
    },
    ModelInfo {
        id: "gpt-4o",
        input_per_million: 2.50,
        output_per_million: 10.0,
        context_window: Some(128_000),
        max_output_tokens: Some(16_384),
    },
    ModelInfo {
        id: "gpt-4o-mini",
        input_per_million: 0.15,
        output_per_million: 0.60,
        context_window: Some(128_000),
        max_output_tokens: Some(16_384),
    },
    ModelInfo {
        id: "o3-mini",
        input_per_million: 1.10,
        output_per_million: 4.40,
        context_window: Some(200_000),
        max_output_tokens: Some(100_000),
    },
    ModelInfo {
        id: "deepseek-chat",
        input_per_million: 0.27,
        output_per_million: 1.10,
        context_window: Some(64_000),
        max_output_tokens: Some(8_192),
    },
    ModelInfo {
        id: "deepseek-reasoner",
        input_per_million: 0.55,
        output_per_million: 2.19,
        context_window: Some(64_000),
        max_output_tokens: Some(8_192),
    },
];

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_known() {
        let info = resolve_model("claude-sonnet-4-20250514").expect("known model");
        assert_eq!(info.id, "claude-sonnet-4-20250514");
        assert!((info.input_per_million - 3.0).abs() < f64::EPSILON);
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
        let info = resolve_model("claude-sonnet-4-20250514").expect("known model");
        assert_eq!(info.context_window, Some(200_000));
        assert_eq!(info.max_output_tokens, Some(64_000));
    }

    #[test]
    fn default_max_output_tokens_known_model() {
        assert_eq!(default_max_output_tokens("claude-sonnet-4-20250514"), Some(64_000));
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
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
            "claude-haiku-3-5-20241022",
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
}
