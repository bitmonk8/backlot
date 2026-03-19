//! Env/path validation for pre-spawn checks.
//!
//! Validates that the child's effective TEMP/TMP/TMPDIR and PATH env vars
//! reference directories the sandbox can actually access.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::command::SandboxCommand;
use crate::error::SandboxError;
use crate::policy::SandboxPolicy;

/// Check that the child's effective TEMP/TMP/TMPDIR and PATH env vars
/// reference directories the sandbox can actually access. Returns
/// `InvalidPolicy` with actionable guidance if any are unreachable.
///
/// A directory is considered accessible if it falls under a policy grant path
/// OR under a platform-implicit path (system dirs each platform auto-mounts
/// or allows by default).
pub fn validate_env_accessibility(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    let implicit = platform_implicit_read_paths();
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
            .filter(|entry| !is_accessible_precanonicalized(entry, &canon_grants, &canon_implicit))
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

/// Directories each platform makes accessible to sandboxed processes
/// regardless of what the policy grants. These are auto-mounted (Linux),
/// allowed by default in the SBPL profile (macOS), or readable by all
/// AppContainer processes (Windows).
pub fn platform_implicit_read_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "linux")]
    {
        for p in &[
            "/lib",
            "/lib64",
            "/usr/lib",
            "/usr/lib64",
            "/usr/lib32",
            "/bin",
            "/usr/bin",
            "/sbin",
            "/usr/sbin",
            "/usr/local/bin",
        ] {
            let path = Path::new(p);
            if path.exists() {
                paths.push(path.to_path_buf());
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        for p in &[
            "/usr/lib",
            "/usr/bin",
            "/bin",
            "/sbin",
            "/usr/sbin",
            "/usr/local/bin",
            "/System/Library",
            "/System/Cryptexes",
        ] {
            let path = Path::new(p);
            if path.exists() {
                paths.push(path.to_path_buf());
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // AppContainer processes can read system directories by default.
        let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
        // sys_root covers all System32 subdirs via starts_with ancestor check.
        paths.push(PathBuf::from(&sys_root));
    }

    paths
}

/// Check if `dir` is accessible using pre-canonicalized grant and implicit paths.
/// Canonicalizes only `dir` (once) instead of re-canonicalizing grants per call.
fn is_accessible_precanonicalized(
    dir: &Path,
    canon_grants: &[PathBuf],
    canon_implicit: &[PathBuf],
) -> bool {
    let resolved_dir =
        std::fs::canonicalize(dir).unwrap_or_else(|_| canonicalize_existing_prefix(dir));

    canon_grants.iter().any(|g| resolved_dir.starts_with(g))
        || canon_implicit.iter().any(|g| resolved_dir.starts_with(g))
}

/// True if `child` is equal to `parent` or a descendant of it.
/// Tries canonicalization first; falls back to lexical comparison.
/// When only one path canonicalizes, resolves the other's existing
/// ancestor prefix to avoid mismatches from symlinks (e.g.,
/// `/var` -> `/private/var` on macOS).
#[cfg(test)]
pub fn path_contains(parent: &Path, child: &Path) -> bool {
    let canon_parent = std::fs::canonicalize(parent);
    let canon_child = std::fs::canonicalize(child);
    if let (Ok(cp), Ok(cc)) = (&canon_parent, &canon_child) {
        return cc.starts_with(cp);
    }
    let np = canon_parent.unwrap_or_else(|_| normalize_lexical(parent));
    let nc = canon_child.unwrap_or_else(|_| canonicalize_existing_prefix(child));
    nc.starts_with(&np)
}

/// Canonicalize the longest existing prefix of `path`, then append
/// the remaining non-existent components. Handles cases where the
/// full path doesn't exist but ancestors contain symlinks.
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut suffix_parts = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut result = canon;
            for part in suffix_parts.into_iter().rev() {
                result.push(part);
            }
            return result;
        }
        match existing.file_name() {
            Some(name) => {
                suffix_parts.push(name.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    normalize_lexical(path)
}

/// Normalize a path lexically: resolve `.` and `..` components, normalize separators.
/// Does NOT touch the filesystem.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    debug_assert!(
        path.is_absolute(),
        "normalize_lexical requires absolute paths"
    );
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {} // skip `.`
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

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
    fn path_contains_equal_paths() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        assert!(path_contains(dir.path(), dir.path()));
    }

    #[test]
    fn path_contains_child_is_descendant() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let child = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&child).expect("create subdirs");
        assert!(path_contains(dir.path(), &child));
    }

    #[test]
    fn path_contains_child_is_not_under_parent() {
        let a = tempfile::TempDir::new().expect("create temp dir");
        let b = tempfile::TempDir::new().expect("create temp dir");
        assert!(!path_contains(a.path(), b.path()));
    }

    #[test]
    fn path_contains_nonexistent_path_uses_lexical_fallback() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        // Child does not exist on disk, so canonicalize will fail.
        // The lexical fallback should still detect it as under `dir`.
        let nonexistent = dir.path().join("does_not_exist").join("nested");
        assert!(path_contains(dir.path(), &nonexistent));
    }

    #[test]
    fn path_contains_partial_canon_uses_available_result() {
        // When parent canonicalizes but child doesn't, the canonicalized
        // parent should still be used for comparison.
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let nonexistent = dir.path().join("no_such_child");
        assert!(path_contains(dir.path(), &nonexistent));
    }

    #[test]
    fn normalize_lexical_resolves_dot() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\.\b");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/./b");

        let result = normalize_lexical(input);

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\b"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/b"));
    }

    #[test]
    fn normalize_lexical_resolves_parent() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\b\..\c");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/b/../c");

        let result = normalize_lexical(input);

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\c"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/c"));
    }

    #[test]
    fn normalize_lexical_plain_absolute_path() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\b\c");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/b/c");

        let result = normalize_lexical(input);

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\b\c"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/b/c"));
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

    /// Default PATH entries must be a subset of platform_implicit_read_paths,
    /// otherwise validate_env_accessibility would reject empty-env configs.
    #[test]
    #[cfg(unix)]
    fn default_path_subset_of_implicit_read_paths() {
        let implicit = platform_implicit_read_paths();
        for entry in DEFAULT_UNIX_PATH.split(':') {
            let entry_path = Path::new(entry);
            if !entry_path.exists() {
                continue;
            }
            let covered = implicit.iter().any(|imp| path_contains(imp, entry_path));
            assert!(
                covered,
                "default PATH entry {entry} is not covered by platform_implicit_read_paths"
            );
        }
    }
}
