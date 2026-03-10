// Per-project configuration: verification steps, model preferences, paths, limits.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Top-level project configuration, serialized as `epic.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default = "default_epic_dir")]
    pub epic_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_fast_model")]
    pub fast: String,
    #[serde(default = "default_balanced_model")]
    pub balanced: String,
    #[serde(default = "default_strong_model")]
    pub strong: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Expose file tools (Read, Write, Edit, Glob, Grep) as separate tool definitions
    /// that forward to nu custom commands. When false, only the `NuShell` tool is offered.
    #[serde(default = "default_file_tool_forwarders")]
    pub file_tool_forwarders: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
const fn default_file_tool_forwarders() -> bool {
    true
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
            file_tool_forwarders: default_file_tool_forwarders(),
        }
    }
}

impl EpicConfig {
    /// Load config from the given path, returning defaults if the file does not exist.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("reading config from {}", path.display())));
            }
        };
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("parsing config from {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate all config fields have reasonable values.
    pub fn validate(&self) -> anyhow::Result<()> {
        let l = &self.limits;

        if l.max_depth < 1 {
            anyhow::bail!("limits.max_depth must be >= 1, got {}", l.max_depth);
        }
        if l.max_depth > 32 {
            anyhow::bail!("limits.max_depth must be <= 32, got {}", l.max_depth);
        }
        if l.max_recovery_rounds < 1 {
            anyhow::bail!(
                "limits.max_recovery_rounds must be >= 1, got {}",
                l.max_recovery_rounds
            );
        }
        if l.retry_budget < 1 {
            anyhow::bail!("limits.retry_budget must be >= 1, got {}", l.retry_budget);
        }
        if l.branch_fix_rounds < 1 {
            anyhow::bail!(
                "limits.branch_fix_rounds must be >= 1, got {}",
                l.branch_fix_rounds
            );
        }
        if l.root_fix_rounds < 1 {
            anyhow::bail!(
                "limits.root_fix_rounds must be >= 1, got {}",
                l.root_fix_rounds
            );
        }
        if l.max_total_tasks < 1 {
            anyhow::bail!(
                "limits.max_total_tasks must be >= 1, got {}",
                l.max_total_tasks
            );
        }
        if l.max_total_tasks > 10_000 {
            anyhow::bail!(
                "limits.max_total_tasks must be <= 10000, got {}",
                l.max_total_tasks
            );
        }

        for (i, step) in self.verification_steps.iter().enumerate() {
            if step.command.is_empty() {
                anyhow::bail!("verification[{i}].command must not be empty");
            }
            if step.timeout < 1 {
                anyhow::bail!(
                    "verification[{}].timeout must be >= 1, got {}",
                    i,
                    step.timeout
                );
            }
        }

        Ok(())
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
        assert!(parsed.agent.file_tool_forwarders);
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
        assert!(config.agent.file_tool_forwarders);
    }

    #[test]
    fn file_tool_forwarders_explicit_false() {
        let config: EpicConfig =
            toml::from_str("[agent]\nfile_tool_forwarders = false\n").unwrap();
        assert!(!config.agent.file_tool_forwarders);
    }

    #[test]
    fn default_max_total_tasks() {
        let config = LimitsConfig::default();
        assert_eq!(config.max_total_tasks, 100);
    }

    #[test]
    fn load_nonexistent_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent").join("epic.toml");
        let config = EpicConfig::load(&path).unwrap();
        assert_eq!(config, EpicConfig::default());
    }

    #[test]
    fn default_config_partial_eq() {
        assert_eq!(EpicConfig::default(), EpicConfig::default());
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

    #[test]
    fn validate_default_config_passes() {
        EpicConfig::default().validate().unwrap();
    }

    #[test]
    fn validate_max_depth_zero() {
        let mut config = EpicConfig::default();
        config.limits.max_depth = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("max_depth"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_max_depth_too_large() {
        let mut config = EpicConfig::default();
        config.limits.max_depth = 33;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("max_depth"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_max_depth_at_upper_bound() {
        let mut config = EpicConfig::default();
        config.limits.max_depth = 32;
        config.validate().unwrap();
    }

    #[test]
    fn validate_max_recovery_rounds_zero() {
        let mut config = EpicConfig::default();
        config.limits.max_recovery_rounds = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("max_recovery_rounds"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_retry_budget_zero() {
        let mut config = EpicConfig::default();
        config.limits.retry_budget = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("retry_budget"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_branch_fix_rounds_zero() {
        let mut config = EpicConfig::default();
        config.limits.branch_fix_rounds = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("branch_fix_rounds"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_root_fix_rounds_zero() {
        let mut config = EpicConfig::default();
        config.limits.root_fix_rounds = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("root_fix_rounds"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_max_total_tasks_zero() {
        let mut config = EpicConfig::default();
        config.limits.max_total_tasks = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("max_total_tasks"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_max_total_tasks_too_large() {
        let mut config = EpicConfig::default();
        config.limits.max_total_tasks = 10_001;
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("max_total_tasks"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn validate_max_total_tasks_at_upper_bound() {
        let mut config = EpicConfig::default();
        config.limits.max_total_tasks = 10_000;
        config.validate().unwrap();
    }

    #[test]
    fn validate_verification_timeout_zero() {
        let mut config = EpicConfig::default();
        config.verification_steps.push(VerificationStep {
            name: "test".into(),
            command: vec!["cargo".into(), "test".into()],
            timeout: 0,
        });
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("timeout"),
            "error should name the field: {err}"
        );
    }

    #[test]
    fn load_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("epic.toml");
        std::fs::write(
            &path,
            r"
[limits]
max_depth = 4
max_total_tasks = 50
",
        )
        .unwrap();
        let config = EpicConfig::load(&path).unwrap();
        assert_eq!(config.limits.max_depth, 4);
        assert_eq!(config.limits.max_total_tasks, 50);
    }

    #[test]
    fn load_malformed_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("epic.toml");
        std::fs::write(&path, "not valid [[ toml %%%").unwrap();
        let err = EpicConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("parsing config"), "{err}");
    }

    #[test]
    fn load_invalid_values_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("epic.toml");
        std::fs::write(&path, "[limits]\nmax_depth = 0\n").unwrap();
        let err = EpicConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("max_depth"), "{err}");
    }

    #[test]
    fn validate_verification_second_step_invalid() {
        let mut config = EpicConfig::default();
        config.verification_steps.push(VerificationStep {
            name: "ok".into(),
            command: vec!["true".into()],
            timeout: 60,
        });
        config.verification_steps.push(VerificationStep {
            name: "bad".into(),
            command: vec!["false".into()],
            timeout: 0,
        });
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("verification[1]"),
            "should report index 1: {err}"
        );
    }

    #[test]
    fn load_directory_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = EpicConfig::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("reading config"), "{err}");
    }

    #[test]
    fn validate_verification_empty_command() {
        let mut config = EpicConfig::default();
        config.verification_steps.push(VerificationStep {
            name: "empty".into(),
            command: vec![],
            timeout: 60,
        });
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("command"),
            "error should mention command: {err}"
        );
    }
}
