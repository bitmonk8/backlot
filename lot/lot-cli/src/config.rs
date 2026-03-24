//! YAML config deserialization and policy construction for the CLI.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(serde::Deserialize, Default)]
pub struct SandboxConfig {
    #[serde(default)]
    pub filesystem: FilesystemConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub environment: EnvironmentConfig,
    #[serde(default)]
    pub process: ProcessConfig,
}

#[derive(serde::Deserialize, Default)]
pub struct FilesystemConfig {
    #[serde(default)]
    pub read: Vec<PathBuf>,
    #[serde(default)]
    pub write: Vec<PathBuf>,
    #[serde(default)]
    pub exec: Vec<PathBuf>,
    #[serde(default)]
    pub deny: Vec<PathBuf>,
    #[serde(default)]
    pub include_platform_exec: bool,
    #[serde(default)]
    pub include_platform_lib: bool,
    #[serde(default)]
    pub include_temp: bool,
}

#[derive(serde::Deserialize, Default)]
pub struct NetworkConfig {
    #[serde(default)]
    pub allow: bool,
}

#[allow(clippy::struct_field_names)]
#[derive(serde::Deserialize, Default)]
pub struct LimitsConfig {
    pub max_memory_bytes: Option<u64>,
    pub max_processes: Option<u32>,
    pub max_cpu_seconds: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub forward_common: bool,
    #[serde(default)]
    pub vars: HashMap<String, String>,
}

#[derive(serde::Deserialize, Default)]
pub struct ProcessConfig {
    pub cwd: Option<PathBuf>,
}

pub fn build_policy(config: &SandboxConfig) -> lot::Result<lot::SandboxPolicy> {
    let mut builder = lot::SandboxPolicyBuilder::new();

    for path in &config.filesystem.read {
        builder = builder.read_path(path)?;
    }
    for path in &config.filesystem.write {
        builder = builder.write_path(path)?;
    }
    for path in &config.filesystem.exec {
        builder = builder.exec_path(path)?;
    }
    for path in &config.filesystem.deny {
        builder = builder.deny_path(path)?;
    }
    if config.filesystem.include_platform_exec {
        builder = builder.include_platform_exec_paths()?;
    }
    if config.filesystem.include_platform_lib {
        builder = builder.include_platform_lib_paths()?;
    }
    if config.filesystem.include_temp {
        builder = builder.include_temp_dirs()?;
    }

    builder = builder.allow_network(config.network.allow);

    if let Some(bytes) = config.limits.max_memory_bytes {
        builder = builder.max_memory_bytes(bytes);
    }
    if let Some(n) = config.limits.max_processes {
        builder = builder.max_processes(n);
    }
    if let Some(secs) = config.limits.max_cpu_seconds {
        builder = builder.max_cpu_seconds(secs);
    }

    builder.build()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn make_temp_dir() -> tempfile::TempDir {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        tempfile::TempDir::new_in(workspace.join("test_tmp")).expect("failed to create temp dir")
    }

    #[test]
    fn build_policy_minimal_config() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config);
        assert!(policy.is_ok(), "minimal config should produce valid policy");
    }

    #[test]
    fn build_policy_empty_config_fails() {
        let config = SandboxConfig::default();
        let policy = build_policy(&config);
        assert!(policy.is_err(), "empty config should fail validation");
    }

    #[test]
    fn build_policy_write_path_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                write: vec![tmp.path().to_path_buf()],
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        let canon = std::fs::canonicalize(tmp.path()).expect("canonicalize temp dir");
        assert!(
            policy.write_paths().contains(&canon),
            "policy write_paths should contain the canonicalized temp dir"
        );
    }

    #[test]
    fn build_policy_network_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                ..FilesystemConfig::default()
            },
            network: NetworkConfig { allow: true },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        assert!(policy.allow_network(), "policy should have network enabled");
    }

    #[test]
    fn build_policy_limits_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                ..FilesystemConfig::default()
            },
            limits: LimitsConfig {
                max_memory_bytes: Some(1_048_576),
                max_processes: Some(10),
                max_cpu_seconds: Some(30),
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        assert_eq!(policy.limits().max_memory_bytes, Some(1_048_576));
        assert_eq!(policy.limits().max_processes, Some(10));
        assert_eq!(policy.limits().max_cpu_seconds, Some(30));
    }

    #[test]
    fn config_deserialization_minimal() {
        let yaml = "filesystem:\n  read:\n    - .\n";
        let config: Result<SandboxConfig, _> = serde_yml::from_str(yaml);
        assert!(config.is_ok(), "minimal YAML should deserialize");
    }

    #[test]
    fn config_deserialization_with_limits() {
        let yaml = r"
filesystem:
  read:
    - .
limits:
  max_memory_bytes: 1048576
  max_processes: 10
";
        let config: SandboxConfig = serde_yml::from_str(yaml).expect("parse YAML");
        assert_eq!(config.limits.max_memory_bytes, Some(1_048_576));
        assert_eq!(config.limits.max_processes, Some(10));
    }

    #[test]
    fn config_deserialization_all_fields() {
        let yaml = r"
filesystem:
  read:
    - /tmp/r
  write:
    - /tmp/w
  exec:
    - /tmp/e
  deny:
    - /tmp/d
  include_platform_exec: true
  include_platform_lib: true
  include_temp: true
network:
  allow: true
limits:
  max_memory_bytes: 1048576
  max_processes: 10
  max_cpu_seconds: 30
environment:
  forward_common: true
  vars:
    FOO: bar
process:
  cwd: /tmp
";
        let config: SandboxConfig = serde_yml::from_str(yaml).expect("parse YAML");
        assert_eq!(config.filesystem.read.len(), 1);
        assert_eq!(config.filesystem.write.len(), 1);
        assert_eq!(config.filesystem.exec.len(), 1);
        assert_eq!(config.filesystem.deny.len(), 1);
        assert!(config.filesystem.include_platform_exec);
        assert!(config.filesystem.include_platform_lib);
        assert!(config.filesystem.include_temp);
        assert!(config.network.allow);
        assert_eq!(config.limits.max_memory_bytes, Some(1_048_576));
        assert_eq!(config.limits.max_processes, Some(10));
        assert_eq!(config.limits.max_cpu_seconds, Some(30));
        assert!(config.environment.forward_common);
        assert_eq!(
            config.environment.vars.get("FOO").map(String::as_str),
            Some("bar")
        );
        assert!(config.process.cwd.is_some());
    }

    #[test]
    fn build_policy_exec_path_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                exec: vec![tmp.path().to_path_buf()],
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        let canon = std::fs::canonicalize(tmp.path()).expect("canonicalize temp dir");
        assert!(
            policy.exec_paths().contains(&canon),
            "policy exec_paths should contain the canonicalized temp dir"
        );
    }

    #[test]
    fn build_policy_deny_path_wired() {
        let tmp = make_temp_dir();
        let deny_dir = tmp.path().join("denied");
        std::fs::create_dir_all(&deny_dir).expect("create deny subdir");
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                deny: vec![deny_dir.clone()],
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        let deny_canon = std::fs::canonicalize(&deny_dir).expect("canonicalize deny dir");
        assert!(
            policy.deny_paths().contains(&deny_canon),
            "policy deny_paths should contain the canonicalized deny dir"
        );
    }

    #[test]
    fn build_policy_include_platform_exec_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                include_platform_exec: true,
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        // Platform exec paths add entries on Unix; on Windows exec_paths may be
        // empty (AppContainer inherits system binary access), but policy builds OK.
        #[cfg(unix)]
        assert!(
            !policy.exec_paths().is_empty(),
            "include_platform_exec should add exec paths on Unix"
        );
        let _ = &policy;
    }

    #[test]
    fn build_policy_include_platform_lib_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                include_platform_lib: true,
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        assert!(
            !policy.read_paths().is_empty(),
            "include_platform_lib should ensure read_paths is non-empty"
        );
    }

    #[test]
    fn build_policy_include_temp_wired() {
        let tmp = make_temp_dir();
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![tmp.path().to_path_buf()],
                include_temp: true,
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        let temp_canon =
            std::fs::canonicalize(std::env::temp_dir()).expect("canonicalize temp dir");
        assert!(
            policy.write_paths().iter().any(|p| p == &temp_canon),
            "include_temp should add system temp dir to write_paths"
        );
    }
}
