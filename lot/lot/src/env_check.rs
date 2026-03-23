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

/// True if the resolved path falls under any deny path.
fn is_denied(resolved: &Path, canon_deny: &[PathBuf]) -> bool {
    canon_deny.iter().any(|d| resolved.starts_with(d))
}

/// Check if `dir` is accessible given pre-canonicalized grant, implicit, and deny paths.
/// Canonicalizes only `dir` (once) instead of re-canonicalizing grants per call.
/// A dir under a deny path is inaccessible even if covered by a grant or implicit path.
fn is_dir_accessible(
    dir: &Path,
    canon_grants: &[PathBuf],
    canon_implicit: &[PathBuf],
    canon_deny: &[PathBuf],
) -> bool {
    // Relative or path-escaping paths cannot match any absolute grant.
    let Ok(resolved_dir) = canonicalize_existing_prefix(dir) else {
        return false;
    };

    if is_denied(&resolved_dir, canon_deny) {
        return false;
    }

    canon_grants.iter().any(|g| resolved_dir.starts_with(g))
        || canon_implicit.iter().any(|g| resolved_dir.starts_with(g))
}

/// Check that the child's effective TEMP/TMP/TMPDIR and PATH env vars
/// reference directories the sandbox can actually access. Returns
/// `InvalidPolicy` with actionable guidance if any are unreachable.
///
/// Two different accessibility rules apply:
/// - **TEMP/TMP/TMPDIR**: must be under a **write path** only (temp dirs need
///   write access). Grant paths and implicit paths are not sufficient.
/// - **PATH entries**: must be under a grant path (read, write, or exec) OR
///   under a platform-implicit path (system dirs each platform auto-mounts).
///
/// Both checks reject directories under deny paths.
pub fn validate_env_accessibility(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<()> {
    // Pre-canonicalize a slice of paths, collecting failures as errors rather
    // than aborting validation so the user sees all problems at once.
    fn canonicalize_or_collect<P: AsRef<Path>>(
        paths: &[P],
        category: &str,
        errors: &mut Vec<String>,
    ) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            let p = p.as_ref();
            match canonicalize_existing_prefix(p) {
                Ok(c) => out.push(c),
                Err(e) => errors.push(format!(
                    "{category} path {} could not be canonicalized: {e}",
                    p.display()
                )),
            }
        }
        out
    }

    let mut errors: Vec<String> = Vec::new();

    let implicit = crate::platform_implicit_paths();
    let grant_paths = policy.grant_paths();

    // Pre-canonicalize grant, implicit, write, and deny paths once to avoid
    // O(P*G) re-canonicalization in inner loops.
    let canon_grants = canonicalize_or_collect(&grant_paths, "grant", &mut errors);
    let canon_implicit = canonicalize_or_collect(&implicit, "implicit", &mut errors);
    let canon_write_paths = canonicalize_or_collect(policy.write_paths(), "write", &mut errors);
    let canon_deny = canonicalize_or_collect(policy.deny_paths(), "deny", &mut errors);

    // TEMP/TMP/TMPDIR must be under a write path (temp dirs need write access)
    // and must not be under a deny path (deny overrides all grants).
    for key in &["TEMP", "TMP", "TMPDIR"] {
        if let Some(val) = resolve_env_value(command, key) {
            let dir = Path::new(&val);
            if !dir.as_os_str().is_empty()
                && !is_dir_accessible(dir, &canon_write_paths, &[], &canon_deny)
            {
                // Distinguish deny-covered from uncovered for actionable diagnostics.
                let Ok(resolved) = canonicalize_existing_prefix(dir) else {
                    errors.push(format!(
                        "{key}={} could not be resolved and will likely be inaccessible \
                         at runtime. Either ensure it exists or override it with \
                         SandboxCommand::env(\"{key}\", <an accessible path>)",
                        dir.display()
                    ));
                    continue;
                };
                if is_denied(&resolved, &canon_deny) {
                    errors.push(format!(
                        "{key}={} is under a deny_path and will be inaccessible at runtime. \
                         Override it with SandboxCommand::env(\"{key}\", <a non-denied path>)",
                        dir.display()
                    ));
                } else {
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

    // PATH entries must be readable (covered by a grant path or platform-implicit)
    // and must not be under a deny path.
    if let Some(val) = resolve_env_value(command, "PATH") {
        let uncovered: Vec<String> = std::env::split_paths(&val)
            .filter(|entry| !entry.as_os_str().is_empty())
            .filter(|entry| !is_dir_accessible(entry, &canon_grants, &canon_implicit, &canon_deny))
            .map(|entry| entry.display().to_string())
            .collect();
        if !uncovered.is_empty() {
            errors.push(format!(
                "{} PATH entries are not accessible to the sandbox (first: {}). \
                 Either add them as read_path/write_path/exec_path or override PATH with \
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
///
/// Uses first-match semantics: on Unix the env Vec becomes envp directly
/// (first-in-Vec = first in envp = what getenv returns), and on Windows
/// `CreateProcessW` also uses the first occurrence in the environment block.
pub fn resolve_env_value(command: &SandboxCommand, key: &str) -> Option<OsString> {
    // Explicit override in command.env takes priority (first match = what the
    // child's getenv/GetEnvironmentVariable will return).
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
    // When command.env is empty on Windows, the child inherits the parent's
    // full environment (CreateProcessW with lpEnvironment=NULL). Fall back
    // to the parent's value so validation catches paths the child will use.
    // On Unix, empty command.env still builds an explicit envp (only a
    // default PATH); the Unix branch below handles that separately.
    // When command.env is non-empty on either platform, only those vars
    // are passed to the child — no parent fallback.
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
    fn validate_env_rejects_temp_under_deny_path() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");
        let denied = write_dir.path().join("denied");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicy::new(
            vec![],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![denied.clone()],
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", &denied);
        #[cfg(target_os = "windows")]
        {
            let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", format!(r"{sys_root}\System32"));
        }
        #[cfg(not(target_os = "windows"))]
        cmd.env("PATH", "/usr/bin");

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("deny_path"),
            "error should mention deny_path: {msg}"
        );
    }

    #[test]
    fn validate_env_rejects_path_entry_under_deny_path() {
        let grant_dir = tempfile::TempDir::new().expect("create temp dir");
        let write_dir = tempfile::TempDir::new().expect("create temp dir");
        let denied = grant_dir.path().join("denied");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicy::new(
            vec![grant_dir.path().to_path_buf()],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![denied.clone()],
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", write_dir.path());
        cmd.env("PATH", &denied);

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PATH"), "error should mention PATH: {msg}");
        assert!(
            msg.contains("not accessible"),
            "error should mention inaccessibility: {msg}"
        );
    }

    #[test]
    fn resolve_env_value_explicit_override() {
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("FOO", "bar");
        assert_eq!(resolve_env_value(&cmd, "FOO"), Some(OsString::from("bar")));
    }

    #[test]
    fn resolve_env_value_missing_key() {
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("OTHER", "val");
        assert_eq!(resolve_env_value(&cmd, "NONEXISTENT"), None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn resolve_env_value_default_path_on_unix() {
        let cmd = SandboxCommand::new("dummy");
        let path = resolve_env_value(&cmd, "PATH");
        assert_eq!(path, Some(OsString::from(DEFAULT_UNIX_PATH)));
    }

    // ── is_dir_accessible ───────────────────────────────────────────

    #[test]
    fn is_dir_accessible_granted() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        assert!(is_dir_accessible(&child, &[canon], &[], &[]));
    }

    #[test]
    fn is_dir_accessible_implicit() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        assert!(is_dir_accessible(&child, &[], &[canon], &[]));
    }

    #[test]
    fn is_dir_accessible_not_covered() {
        let a = tempfile::TempDir::new().expect("create temp dir");
        let b = tempfile::TempDir::new().expect("create temp dir");
        let canon_a = std::fs::canonicalize(a.path()).expect("canonicalize");
        assert!(!is_dir_accessible(b.path(), &[canon_a], &[], &[]));
    }

    #[test]
    fn is_dir_accessible_denied_subtree() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let grant = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let denied = dir.path().join("secret");
        let query = dir.path().join("secret").join("deep");
        std::fs::create_dir_all(&query).expect("create dirs");
        let canon_deny = std::fs::canonicalize(&denied).expect("canonicalize");
        // Grant covers parent, deny carves out subtree — query inside denied subtree.
        assert!(!is_dir_accessible(
            &query,
            std::slice::from_ref(&grant),
            &[],
            std::slice::from_ref(&canon_deny),
        ));
        // Sibling outside denied subtree is still accessible.
        let sibling = dir.path().join("allowed");
        std::fs::create_dir(&sibling).expect("create sibling");
        assert!(is_dir_accessible(
            &sibling,
            std::slice::from_ref(&grant),
            &[],
            std::slice::from_ref(&canon_deny),
        ));
    }

    #[test]
    fn is_dir_accessible_denied_overrides_implicit() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        // Implicit but also denied — deny wins.
        assert!(!is_dir_accessible(
            &child,
            &[],
            std::slice::from_ref(&canon),
            std::slice::from_ref(&canon),
        ));
    }

    /// Default PATH entries must be a subset of platform_implicit_paths,
    /// otherwise validate_env_accessibility would reject empty-env configs.
    #[test]
    #[cfg(unix)]
    fn default_path_subset_of_implicit_paths() {
        use crate::path_util::is_descendant_or_equal;
        let implicit = crate::platform_implicit_paths();
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
                "default PATH entry {entry} is not covered by platform_implicit_paths"
            );
        }
    }

    #[test]
    fn validate_env_reports_uncanonicalizeable_policy_path() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");

        // Relative path triggers canonicalize_existing_prefix failure
        // (normalize_lexical rejects non-absolute paths).
        let bogus = PathBuf::from("relative/not/absolute");

        let policy = SandboxPolicy::new(
            vec![bogus],
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

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("could not be canonicalized"),
            "error should report canonicalization failure: {msg}"
        );
        assert!(
            msg.contains("grant"),
            "error should identify the path category: {msg}"
        );
    }

    #[test]
    fn validate_env_reports_unresolvable_temp() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );

        // Relative TEMP triggers canonicalize_existing_prefix failure in
        // the re-canonicalization path (is_dir_accessible returns false,
        // then the diagnostic re-canonicalization also fails).
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", "relative/not/absolute");
        #[cfg(target_os = "windows")]
        {
            let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", format!(r"{sys_root}\System32"));
        }
        #[cfg(not(target_os = "windows"))]
        cmd.env("PATH", "/usr/bin");

        let err = validate_env_accessibility(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("could not be resolved"),
            "error should report resolution failure: {msg}"
        );
        assert!(msg.contains("TEMP"), "error should mention TEMP: {msg}");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn resolve_env_value_windows_nonempty_env_suppresses_fallback() {
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("OTHER", "val");
        // Non-empty env means no parent fallback — SYSTEMROOT should not resolve.
        assert_eq!(
            resolve_env_value(&cmd, "SYSTEMROOT"),
            None,
            "non-empty env should suppress parent environment fallback"
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn resolve_env_value_windows_case_insensitive() {
        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("temp", r"C:\MyTemp");
        // Lookup with uppercase key should match the lowercase entry.
        assert_eq!(
            resolve_env_value(&cmd, "TEMP"),
            Some(OsString::from(r"C:\MyTemp"))
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn resolve_env_value_windows_inherits_parent_env() {
        // Empty command.env triggers parent-environment fallback on Windows.
        let cmd = SandboxCommand::new("dummy");
        let val = resolve_env_value(&cmd, "SYSTEMROOT");
        assert!(
            val.is_some(),
            "SYSTEMROOT should be inherited from the parent environment"
        );
    }
}
