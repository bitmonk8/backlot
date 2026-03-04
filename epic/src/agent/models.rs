// Model selection and escalation: Haiku -> Sonnet -> Opus.

use crate::task::Model;

/// Flick model identifier string for a given model tier.
pub const fn flick_model_id(model: Model) -> &'static str {
    match model {
        Model::Haiku => "claude-haiku-4-5-20251001",
        Model::Sonnet => "claude-sonnet-4-6",
        Model::Opus => "claude-opus-4-6",
    }
}

/// Default max token budget for a given model tier.
pub const fn default_max_tokens(model: Model) -> u32 {
    match model {
        Model::Haiku | Model::Sonnet => 8192,
        Model::Opus => 16384,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_are_valid() {
        assert_eq!(flick_model_id(Model::Haiku), "claude-haiku-4-5-20251001");
        assert_eq!(flick_model_id(Model::Sonnet), "claude-sonnet-4-6");
        assert_eq!(flick_model_id(Model::Opus), "claude-opus-4-6");
    }

    #[test]
    fn token_limits() {
        assert_eq!(default_max_tokens(Model::Haiku), 8192);
        assert_eq!(default_max_tokens(Model::Sonnet), 8192);
        assert_eq!(default_max_tokens(Model::Opus), 16384);
    }
}
