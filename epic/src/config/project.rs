// Per-project configuration: verification steps, model preferences, paths, limits.

use serde::{Deserialize, Serialize};

/// Top-level project configuration, serialized as `epic.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpicConfig {
    #[serde(default)]
    pub project: ProjectConfig,

    #[serde(default)]
    pub models: ModelConfig,

    #[serde(default)]
    pub limits: LimitsConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default, rename = "verification")]
    pub verification_steps: Vec<VerificationStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default = "default_epic_dir")]
    pub epic_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_fast_model")]
    pub fast: String,
    #[serde(default = "default_balanced_model")]
    pub balanced: String,
    #[serde(default = "default_strong_model")]
    pub strong: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_max_recovery_rounds")]
    pub max_recovery_rounds: u32,
    #[serde(default = "default_retry_budget")]
    pub retry_budget: u32,
    #[serde(default = "default_branch_fix_rounds")]
    pub branch_fix_rounds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default = "default_flick_path")]
    pub flick_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationStep {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
}

// Defaults

fn default_root() -> String { ".".into() }
fn default_epic_dir() -> String { ".epic".into() }
fn default_fast_model() -> String { "haiku-4.5".into() }
fn default_balanced_model() -> String { "sonnet-4.5".into() }
fn default_strong_model() -> String { "opus-4.6".into() }
const fn default_max_depth() -> u32 { 8 }
const fn default_max_recovery_rounds() -> u32 { 2 }
const fn default_retry_budget() -> u32 { 3 }
const fn default_branch_fix_rounds() -> u32 { 3 }
fn default_runtime() -> String { "flick".into() }
fn default_flick_path() -> String { "flick".into() }
const fn default_timeout() -> u32 { 300 }

impl Default for ProjectConfig {
    fn default() -> Self {
        Self { root: default_root(), epic_dir: default_epic_dir() }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            fast: default_fast_model(),
            balanced: default_balanced_model(),
            strong: default_strong_model(),
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            max_recovery_rounds: default_max_recovery_rounds(),
            retry_budget: default_retry_budget(),
            branch_fix_rounds: default_branch_fix_rounds(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { runtime: default_runtime(), flick_path: default_flick_path() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips() {
        let config = EpicConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: EpicConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.limits.max_depth, 8);
        assert_eq!(parsed.models.fast, "haiku-4.5");
    }

    #[test]
    fn parse_with_verification_steps() {
        let toml_str = r#"
[[verification]]
name = "Build"
command = ["cargo", "build"]
timeout = 300

[[verification]]
name = "Test"
command = ["cargo", "test"]
timeout = 600
"#;
        let config: EpicConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.verification_steps.len(), 2);
        assert_eq!(config.verification_steps[0].name, "Build");
        assert_eq!(config.verification_steps[1].command, vec!["cargo", "test"]);
        assert_eq!(config.verification_steps[1].timeout, 600);
    }

    #[test]
    fn parse_minimal_config() {
        let config: EpicConfig = toml::from_str("").unwrap();
        assert_eq!(config.limits.retry_budget, 3);
        assert!(config.verification_steps.is_empty());
    }
}
