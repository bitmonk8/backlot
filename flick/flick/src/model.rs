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

/// Map reasoning level to Anthropic `budget_tokens`.
pub const fn anthropic_budget_tokens(level: ReasoningLevel) -> u32 {
    match level {
        ReasoningLevel::Minimal => 1024,
        ReasoningLevel::Low => 4096,
        ReasoningLevel::Medium => 10_000,
        ReasoningLevel::High => 32_000,
    }
}

/// Map reasoning level to `OpenAI` `reasoning_effort` string.
pub const fn openai_reasoning_effort(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

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
}
