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

    #[serde(default, rename = "verification")]
    pub verification_steps: Vec<VerificationStep>,

    #[serde(default)]
    pub vault: VaultConfig,
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
pub struct VerificationStep {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_vault_storage")]
    pub storage: String,

    #[serde(default = "default_vault_bootstrap_model")]
    pub bootstrap_model: String,

    #[serde(default = "default_vault_query_model")]
    pub query_model: String,

    #[serde(default = "default_vault_record_model")]
    pub record_model: String,

    #[serde(default = "default_vault_reorganize_model")]
    pub reorganize_model: String,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage: default_vault_storage(),
            bootstrap_model: default_vault_bootstrap_model(),
            query_model: default_vault_query_model(),
            record_model: default_vault_record_model(),
            reorganize_model: default_vault_reorganize_model(),
        }
    }
}

// Defaults

fn default_vault_storage() -> String {
    ".epic/docs".into()
}
fn default_vault_bootstrap_model() -> String {
    "balanced".into()
}
fn default_vault_query_model() -> String {
    "fast".into()
}
fn default_vault_record_model() -> String {
    "fast".into()
}
fn default_vault_reorganize_model() -> String {
    "balanced".into()
}

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

impl ModelConfig {
    /// Look up the model name for a given tier.
    pub fn name_for(&self, model: crate::task::Model) -> &str {
        match model {
            crate::task::Model::Haiku => &self.fast,
            crate::task::Model::Sonnet => &self.balanced,
            crate::task::Model::Opus => &self.strong,
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

        if self.vault.enabled {
            if self.vault.storage.is_empty() {
                anyhow::bail!("vault.storage must not be empty when vault is enabled");
            }
            for (field, val) in [
                ("bootstrap_model", &self.vault.bootstrap_model),
                ("query_model", &self.vault.query_model),
                ("record_model", &self.vault.record_model),
                ("reorganize_model", &self.vault.reorganize_model),
            ] {
                if val.is_empty() {
                    anyhow::bail!("vault.{field} must not be empty when vault is enabled");
                }
            }
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
    fn load_nonexistent_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent").join("epic.toml");
        let config = EpicConfig::load(&path).unwrap();
        assert_eq!(config, EpicConfig::default());
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
    fn validate_zero_fields_rejected() {
        #[allow(clippy::type_complexity)]
        let cases: &[(&str, fn(&mut LimitsConfig))] = &[
            ("max_depth", |l| l.max_depth = 0),
            ("max_recovery_rounds", |l| l.max_recovery_rounds = 0),
            ("retry_budget", |l| l.retry_budget = 0),
            ("branch_fix_rounds", |l| l.branch_fix_rounds = 0),
            ("root_fix_rounds", |l| l.root_fix_rounds = 0),
            ("max_total_tasks", |l| l.max_total_tasks = 0),
        ];
        for (field, mutate) in cases {
            let mut config = EpicConfig::default();
            mutate(&mut config.limits);
            let err = config.validate().unwrap_err().to_string();
            assert!(err.contains(field), "{field}: unexpected error: {err}");
        }
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
    fn legacy_agent_file_tool_forwarders_field_ignored() {
        let toml_str = r"
[agent]
file_tool_forwarders = true
";
        // serde should silently ignore the unknown [agent] section
        let config: EpicConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config, EpicConfig::default());
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

    #[test]
    fn vault_config_defaults() {
        let config = VaultConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.storage, ".epic/docs");
        assert_eq!(config.bootstrap_model, "balanced");
        assert_eq!(config.query_model, "fast");
        assert_eq!(config.record_model, "fast");
        assert_eq!(config.reorganize_model, "balanced");
    }

    #[test]
    fn vault_config_round_trips() {
        let toml_str = r#"
[vault]
enabled = true
storage = ".epic/knowledge"
bootstrap_model = "strong"
query_model = "fast"
record_model = "fast"
reorganize_model = "balanced"
"#;
        let config: EpicConfig = toml::from_str(toml_str).unwrap();
        assert!(config.vault.enabled);
        assert_eq!(config.vault.storage, ".epic/knowledge");
        assert_eq!(config.vault.bootstrap_model, "strong");
    }

    #[test]
    fn vault_disabled_by_default_in_empty_config() {
        let config: EpicConfig = toml::from_str("").unwrap();
        assert!(!config.vault.enabled);
    }

    #[test]
    fn vault_enabled_empty_storage_fails_validation() {
        let mut config = EpicConfig::default();
        config.vault.enabled = true;
        config.vault.storage = String::new();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("vault.storage"), "{err}");
    }

    #[test]
    fn vault_enabled_empty_model_fails_validation() {
        let mut config = EpicConfig::default();
        config.vault.enabled = true;
        config.vault.query_model = String::new();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("query_model"), "{err}");
    }

    #[test]
    fn vault_disabled_skips_model_validation() {
        let mut config = EpicConfig::default();
        config.vault.query_model = String::new();
        config.validate().unwrap();
    }
}
