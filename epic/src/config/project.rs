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
    #[serde(default = "default_root_fix_rounds")]
    pub root_fix_rounds: u32,
    #[serde(default = "default_max_total_tasks")]
    pub max_total_tasks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationStep {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
}

// Defaults

fn default_root() -> String {
    ".".into()
}
fn default_epic_dir() -> String {
    ".epic".into()
}
fn default_fast_model() -> String {
    "claude-haiku-4-5-20251001".into()
}
fn default_balanced_model() -> String {
    "claude-sonnet-4-6".into()
}
fn default_strong_model() -> String {
    "claude-opus-4-6".into()
}
const fn default_max_depth() -> u32 {
    8
}
const fn default_max_recovery_rounds() -> u32 {
    2
}
const fn default_retry_budget() -> u32 {
    3
}
const fn default_branch_fix_rounds() -> u32 {
    3
}
const fn default_root_fix_rounds() -> u32 {
    4
}
const fn default_max_total_tasks() -> u32 {
    100
}
fn default_runtime() -> String {
    "flick".into()
}
const fn default_timeout() -> u32 {
    300
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            root: default_root(),
            epic_dir: default_epic_dir(),
        }
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
            root_fix_rounds: default_root_fix_rounds(),
            max_total_tasks: default_max_total_tasks(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
        }
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
        assert_eq!(parsed.models.fast, "claude-haiku-4-5-20251001");
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

    #[test]
    fn default_max_total_tasks() {
        let config = LimitsConfig::default();
        assert_eq!(config.max_total_tasks, 100);
    }

    #[test]
    fn max_total_tasks_round_trips() {
        let toml_str = r"
[limits]
max_total_tasks = 42
";
        let config: EpicConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.limits.max_total_tasks, 42);

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: EpicConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.limits.max_total_tasks, 42);
    }
}
