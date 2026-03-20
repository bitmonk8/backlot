//! Env/path validation for pre-spawn checks.
//!
//! Validates that the child's effective TEMP/TMP/TMPDIR and PATH env vars
//! reference directories the sandbox can actually access.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::command::SandboxCommand;
use crate::error::SandboxError;
use crate::path_util::canonicalize_existing_prefix;
use crate::policy::SandboxPolicy;

/// Check if `dir` is accessible given pre-canonicalized grant and implicit paths.
/// Canonicalizes only `dir` (once) instead of re-canonicalizing grants per call.
fn is_dir_accessible(dir: &Path, canon_grants: &[PathBuf], canon_implicit: &[PathBuf]) -> bool {
    let resolved_dir =
        std::fs::canonicalize(dir).unwrap_or_else(|_| canonicalize_existing_prefix(dir));

    canon_grants.iter().any(|g| resolved_dir.starts_with(g))
        || canon_implicit.iter().any(|g| resolved_dir.starts_with(g))
}

/// Check that the child's effective TEMP/TMP/TMPDIR and PATH env vars
/// reference directories the sandbox can actually access. Returns
/// `InvalidPolicy` with actionable guidance if any are unreachable.
///
/// A directory is considered accessible if it falls under a policy grant path
/// OR under a platform-implicit path (system dirs each platform auto-mounts
/// or allows by default).
pub fn validate_env_accessibility(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    let implicit = crate::platform_implicit_read_paths();
    let grant_paths = policy.grant_paths();

    // Pre-canonicalize grant, implicit, and write paths once to avoid
    // O(P*G) re-canonicalization in inner loops.
    let canon_grants: Vec<PathBuf> = grant_paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
        .collect();
    let canon_implicit: Vec<PathBuf> = implicit
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let canon_write_paths: Vec<PathBuf> = policy
        .write_paths()
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();

    // TEMP/TMP/TMPDIR must be under a write path (temp dirs need write access).
    // Platform-implicit paths are read-only, so they don't satisfy temp.
    for key in &["TEMP", "TMP", "TMPDIR"] {
        if let Some(val) = effective_env(command, key) {
            let dir = Path::new(&val);
            if !dir.as_os_str().is_empty() {
                let resolved = std::fs::canonicalize(dir)
                    .unwrap_or_else(|_| canonicalize_existing_prefix(dir));
                if !canon_write_paths.iter().any(|wp| resolved.starts_with(wp)) {
                    errors.push(format!(
                        "{key}={} is not covered by any write_path in the policy. \
                         Either add it as a write_path or override it with \
                         SandboxCommand::env(\"{key}\", <a granted path>)",
                        dir.display()
                    ));
                }
            }
        }
    }

    // PATH entries must be readable (covered by a grant path or platform-implicit).
    if let Some(val) = effective_env(command, "PATH") {
        let uncovered: Vec<String> = std::env::split_paths(&val)
            .filter(|entry| !entry.as_os_str().is_empty())
            .filter(|entry| !is_dir_accessible(entry, &canon_grants, &canon_implicit))
            .map(|entry| entry.display().to_string())
            .collect();
        if !uncovered.is_empty() {
            errors.push(format!(
                "{} PATH entries are not accessible to the sandbox (first: {}). \
                 Either add them as read_path/exec_path or override PATH with \
                 SandboxCommand::env(\"PATH\", <accessible paths only>)",
                uncovered.len(),
                uncovered[0]
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SandboxError::InvalidPolicy(errors.join("; ")))
    }
}

/// Resolve the effective value of an env var as the child will see it.
pub fn effective_env(command: &SandboxCommand, key: &str) -> Option<OsString> {
    // Explicit override in command.env takes priority.
    for (k, v) in &command.env {
        let matches = {
            #[cfg(target_os = "windows")]
            {
                k.eq_ignore_ascii_case(std::ffi::OsStr::new(key))
            }
            #[cfg(not(target_os = "windows"))]
            {
                *k == *key
            }
        };
        if matches {
            return Some(v.clone());
        }
    }
    // Inherited env: Windows inherits parent env when command.env is empty.
    // Unix builds an explicit envp — no inheritance, but a default PATH.
    // Intentional: on Windows with empty env, this reads the parent's
    // TEMP/TMP (typically C:\Users\...\AppData\Local\Temp) and requires
    // it in write_paths. Callers must either add system temp as a
    // write_path or override TEMP/TMP via SandboxCommand::env().
    #[cfg(target_os = "windows")]
    if command.env.is_empty() {
        return std::env::var_os(key);
    }
    #[cfg(not(target_os = "windows"))]
    if command.env.is_empty() && key == "PATH" {
        return Some(OsString::from(DEFAULT_UNIX_PATH));
    }
    None
}

/// Default PATH for Unix when no env is specified.
#[cfg(not(target_os = "windows"))]
pub const DEFAULT_UNIX_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::command::SandboxCommand;
    use crate::policy::ResourceLimits;

    #[test]
    fn validate_env_ok_when_temp_in_write_path() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");
        let read_dir = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![read_dir.path().to_path_buf()],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", write_dir.path());
        #[cfg(target_os = "windows")]
        {
            let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", format!(r"{sys_root}\System32"));
        }
        #[cfg(not(target_os = "windows"))]
        cmd.env("PATH", "/usr/bin");

        assert!(
            validate_env_accessibility(&policy, &cmd).is_ok(),
            "TEMP in write_path should pass"
        );
    }

    #[test]
    fn validate_env_rejects_temp_outside_write_paths() {
        let read_dir = tempfile::TempDir::new().expect("create temp dir");
        let uncovered = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![read_dir.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", uncovered.path());
        #[cfg(target_os = "windows")]
        {
            let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", format!(r"{sys_root}\System32"));
        }
        #[cfg(not(target_os = "windows"))]
        cmd.env("PATH", "/usr/bin");

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TEMP"), "error should mention TEMP: {msg}");
    }

    #[test]
    fn validate_env_rejects_uncovered_path_entry() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");
        let uncovered = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", write_dir.path());
        cmd.env("PATH", uncovered.path());

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PATH"), "error should mention PATH: {msg}");
    }

    #[test]
    fn validate_env_accumulates_multiple_errors() {
        let read_dir = tempfile::TempDir::new().expect("create temp dir");
        let bad_temp = tempfile::TempDir::new().expect("create temp dir");
        let bad_path = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![read_dir.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", bad_temp.path());
        cmd.env("PATH", bad_path.path());

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TEMP"), "error should mention TEMP: {msg}");
        assert!(msg.contains("PATH"), "error should mention PATH: {msg}");
    }

    #[test]
    fn effective_env_explicit_override() {
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("FOO", "bar");
        assert_eq!(effective_env(&cmd, "FOO"), Some(OsString::from("bar")));
    }

    #[test]
    fn effective_env_missing_key() {
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("OTHER", "val");
        assert_eq!(effective_env(&cmd, "NONEXISTENT"), None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn effective_env_default_path_on_unix() {
        let cmd = SandboxCommand::new("dummy");
        let path = effective_env(&cmd, "PATH");
        assert_eq!(path, Some(OsString::from(DEFAULT_UNIX_PATH)));
    }

    // ── is_dir_accessible ───────────────────────────────────────────

    #[test]
    fn is_dir_accessible_granted() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        assert!(is_dir_accessible(&child, &[canon], &[]));
    }

    #[test]
    fn is_dir_accessible_implicit() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        assert!(is_dir_accessible(&child, &[], &[canon]));
    }

    #[test]
    fn is_dir_accessible_not_covered() {
        let a = tempfile::TempDir::new().expect("create temp dir");
        let b = tempfile::TempDir::new().expect("create temp dir");
        let canon_a = std::fs::canonicalize(a.path()).expect("canonicalize");
        assert!(!is_dir_accessible(b.path(), &[canon_a], &[]));
    }

    /// Default PATH entries must be a subset of platform_implicit_read_paths,
    /// otherwise validate_env_accessibility would reject empty-env configs.
    #[test]
    #[cfg(unix)]
    fn default_path_subset_of_implicit_read_paths() {
        use crate::path_util::is_descendant_or_equal;
        let implicit = crate::platform_implicit_read_paths();
        for entry in DEFAULT_UNIX_PATH.split(':') {
            let entry_path = Path::new(entry);
            if !entry_path.exists() {
                continue;
            }
            let covered = implicit
                .iter()
                .any(|imp| is_descendant_or_equal(imp, entry_path));
            assert!(
                covered,
                "default PATH entry {entry} is not covered by platform_implicit_read_paths"
            );
        }
    }
}
