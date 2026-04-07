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
    #[serde(default, rename = "ipc-sem")]
    pub ipc_semaphore: bool,
    #[serde(default, rename = "ipc-shm")]
    pub ipc_shm: bool,
}

/// Expand `${VAR}` references in `s` using the process environment.
/// Returns `Err` if any referenced variable is not set or the value is not valid UTF-8.
pub fn expand_vars(s: &str) -> Result<String, String> {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let end = after_open
            .find('}')
            .ok_or_else(|| format!("unclosed '${{' in: {s}"))?;
        let name = &after_open[..end];
        let value = std::env::var(name)
            .map_err(|_| format!("undefined env var: {name}"))?;
        result.push_str(&value);
        rest = &after_open[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

pub fn expand_path(p: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let s = p.to_str().ok_or_else(|| "path is not UTF-8".to_string())?;
    expand_vars(s).map(std::path::PathBuf::from)
}

pub fn build_policy(config: &SandboxConfig) -> lot::Result<lot::SandboxPolicy> {
    let mut builder = lot::SandboxPolicyBuilder::new();

    for path in &config.filesystem.read {
        let expanded = expand_path(path).map_err(lot::SandboxError::Setup)?;
        builder = builder.read_path(&expanded)?;
    }
    for path in &config.filesystem.write {
        let expanded = expand_path(path).map_err(lot::SandboxError::Setup)?;
        builder = builder.write_path(&expanded)?;
    }
    for path in &config.filesystem.exec {
        let expanded = expand_path(path).map_err(lot::SandboxError::Setup)?;
        builder = builder.exec_path(&expanded)?;
    }
    for path in &config.filesystem.deny {
        let expanded = expand_path(path).map_err(lot::SandboxError::Setup)?;
        builder = builder.deny_path(&expanded)?;
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
    builder = builder.allow_ipc_semaphore(config.process.ipc_semaphore);
    builder = builder.allow_ipc_shm(config.process.ipc_shm);

    builder.build()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // --- expand_vars tests ---

    #[test]
    fn expand_vars_basic() {
        // Use PATH which is always present in the test environment.
        let path_val = std::env::var("PATH").expect("PATH must be set");
        let result = expand_vars("${PATH}/bin").unwrap();
        assert_eq!(result, format!("{path_val}/bin"));
    }

    #[test]
    fn expand_vars_undefined_errors() {
        // LOT_GUARANTEED_ABSENT is never set by this test suite.
        let err = expand_vars("${LOT_GUARANTEED_ABSENT}").unwrap_err();
        assert!(err.contains("LOT_GUARANTEED_ABSENT"), "error should name the variable");
    }

    #[test]
    fn expand_vars_no_placeholders() {
        let result = expand_vars("/plain/path").unwrap();
        assert_eq!(result, "/plain/path");
    }

    #[test]
    fn expand_vars_multiple() {
        // Use two vars that are always present.
        let home = std::env::var("HOME").expect("HOME must be set");
        let path = std::env::var("PATH").expect("PATH must be set");
        let result = expand_vars("${HOME}:${PATH}").unwrap();
        assert_eq!(result, format!("{home}:{path}"));
    }

    #[test]
    fn expand_vars_malformed_no_close_brace() {
        let err = expand_vars("${UNCLOSED").unwrap_err();
        assert!(err.contains("unclosed"), "error should mention unclosed");
    }

    #[test]
    fn build_policy_path_with_env_var() {
        // Use CARGO_MANIFEST_DIR which cargo sets for all tests.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
        let config = SandboxConfig {
            filesystem: FilesystemConfig {
                read: vec![std::path::PathBuf::from("${CARGO_MANIFEST_DIR}")],
                ..FilesystemConfig::default()
            },
            ..SandboxConfig::default()
        };
        let policy = build_policy(&config).expect("build_policy should succeed");
        let canon = std::fs::canonicalize(&manifest_dir).expect("canonicalize");
        assert!(
            policy.read_paths().contains(&canon),
            "policy read_paths should contain the expanded path"
        );
    }

    fn make_temp_dir() -> tempfile::TempDir {
        let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
        tempfile::TempDir::new_in(test_tmp).expect("failed to create temp dir")
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
    fn config_deserialization_minimal() {
        let yaml = "filesystem:\n  read:\n    - .\n";
        let config: Result<SandboxConfig, _> = serde_yml::from_str(yaml);
        assert!(config.is_ok(), "minimal YAML should deserialize");
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
