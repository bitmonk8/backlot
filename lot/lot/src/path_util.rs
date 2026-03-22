//! Shared path utilities: ancestry checks, lexical normalization,
//! partial canonicalization.

use std::path::{Path, PathBuf};

use crate::error::SandboxError;

/// True if `child` is equal to `parent` or a descendant of it.
/// Tries canonicalization first; falls back to partial canonicalization
/// via `canonicalize_existing_prefix`.
/// When only one path canonicalizes, resolves the other's existing
/// ancestor prefix to avoid mismatches from symlinks (e.g.,
/// `/var` -> `/private/var` on macOS).
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub fn is_descendant_or_equal(parent: &Path, child: &Path) -> bool {
    let canon_parent = std::fs::canonicalize(parent);
    let canon_child = std::fs::canonicalize(child);
    if let (Ok(cp), Ok(cc)) = (&canon_parent, &canon_child) {
        return cc.starts_with(cp);
    }
    // Both paths come from test code with known-good absolute paths.
    let np = canon_parent.unwrap_or_else(|_| canonicalize_existing_prefix(parent).unwrap());
    let nc = canon_child.unwrap_or_else(|_| canonicalize_existing_prefix(child).unwrap());
    nc.starts_with(&np)
}

/// Returns true if `parent` is a strict prefix of `child` (directory ancestry).
pub fn is_strict_parent_of(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent) && child != parent
}

/// Canonicalize the longest existing prefix of `path`, then append
/// the remaining non-existent components. Handles cases where the
/// full path doesn't exist but ancestors contain symlinks.
///
/// # Errors
///
/// Returns `SandboxError::InvalidPolicy` if the path is relative or
/// contains `..` components that would escape the root.
pub fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, SandboxError> {
    let mut existing = path.to_path_buf();
    let mut suffix_parts = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut result = canon;
            for part in suffix_parts.into_iter().rev() {
                result.push(part);
            }
            return Ok(result);
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

/// Best-effort path resolution: try full canonicalization, fall back to
/// prefix canonicalization, then to the original path unchanged.
pub fn canonicalize_best_effort(path: &Path) -> PathBuf {
    canonicalize_existing_prefix(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Normalize a path lexically: resolve `.` and `..` components, normalize separators.
/// Does NOT touch the filesystem.
///
/// # Errors
///
/// Returns `SandboxError::InvalidPolicy` if the path is not absolute or
/// if a `..` component would escape past the root prefix.
pub fn normalize_lexical(path: &Path) -> Result<PathBuf, SandboxError> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(SandboxError::InvalidPolicy(format!(
            "normalize_lexical requires an absolute path, got: {}",
            path.display()
        )));
    }
    let mut depth: usize = 0;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {} // skip `.`
            Component::ParentDir => {
                if depth == 0 {
                    return Err(SandboxError::InvalidPolicy(format!(
                        "path escapes root via excess `..`: {}",
                        path.display()
                    )));
                }
                depth -= 1;
                out.pop();
            }
            Component::Normal(_) => {
                depth += 1;
                out.push(comp);
            }
            // RootDir, Prefix — structural components that don't increase depth
            other => out.push(other),
        }
    }
    Ok(out)
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ── is_descendant_or_equal ──────────────────────────────────────

    #[test]
    fn descendant_or_equal_equal_paths() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        assert!(is_descendant_or_equal(dir.path(), dir.path()));
    }

    #[test]
    fn descendant_or_equal_child_is_descendant() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let child = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&child).expect("create subdirs");
        assert!(is_descendant_or_equal(dir.path(), &child));
    }

    #[test]
    fn descendant_or_equal_child_is_not_under_parent() {
        let a = tempfile::TempDir::new().expect("create temp dir");
        let b = tempfile::TempDir::new().expect("create temp dir");
        assert!(!is_descendant_or_equal(a.path(), b.path()));
    }

    #[test]
    fn descendant_or_equal_nonexistent_uses_lexical_fallback() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let nonexistent = dir.path().join("does_not_exist").join("nested");
        assert!(is_descendant_or_equal(dir.path(), &nonexistent));
    }

    #[test]
    fn descendant_or_equal_partial_canon_uses_available_result() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let nonexistent = dir.path().join("no_such_child");
        assert!(is_descendant_or_equal(dir.path(), &nonexistent));
    }

    /// macOS: /var is a symlink to /private/var. Verify symlink resolution.
    #[test]
    #[cfg(target_os = "macos")]
    fn descendant_or_equal_macos_var_symlink() {
        assert!(is_descendant_or_equal(
            Path::new("/var"),
            Path::new("/var/tmp")
        ));
        assert!(is_descendant_or_equal(
            Path::new("/private/var"),
            Path::new("/var/tmp")
        ));
    }

    /// When parent fails to canonicalize but child succeeds, the fallback
    /// uses `canonicalize_existing_prefix` for the parent.
    #[test]
    fn descendant_or_equal_reverse_partial_canon() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        // Use a nonexistent parent that shares the same prefix
        let fake_parent = dir.path().join("nonexistent_parent");
        // child is NOT under fake_parent
        assert!(!is_descendant_or_equal(&fake_parent, &child));

        // Now test where the nonexistent "parent" is actually a prefix path
        // that doesn't exist but whose existing prefix resolves correctly.
        // The child's real path starts with dir, not with fake_parent.
        let deep_child = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&deep_child).expect("create deep child");
        // sub is a real dir, deep_child is under it
        assert!(is_descendant_or_equal(&child, &deep_child));
    }

    // ── is_strict_parent_of ──────────────────────────────────────────

    #[test]
    fn strict_parent_ancestor_returns_true() {
        assert!(is_strict_parent_of(
            Path::new(if cfg!(windows) { r"C:\a" } else { "/a" }),
            Path::new(if cfg!(windows) { r"C:\a\b" } else { "/a/b" }),
        ));
    }

    #[test]
    fn strict_parent_equal_paths_returns_false() {
        assert!(!is_strict_parent_of(
            Path::new(if cfg!(windows) { r"C:\a\b" } else { "/a/b" }),
            Path::new(if cfg!(windows) { r"C:\a\b" } else { "/a/b" }),
        ));
    }

    #[test]
    fn strict_parent_unrelated_paths_returns_false() {
        assert!(!is_strict_parent_of(
            Path::new(if cfg!(windows) { r"C:\x" } else { "/x" }),
            Path::new(if cfg!(windows) { r"C:\y\z" } else { "/y/z" }),
        ));
    }

    #[test]
    fn strict_parent_similar_prefix_not_parent() {
        // "/a/b" is NOT a parent of "/a/bc" — they share a string prefix
        // but not a component-wise prefix.
        assert!(!is_strict_parent_of(
            Path::new(if cfg!(windows) { r"C:\a\b" } else { "/a/b" }),
            Path::new(if cfg!(windows) { r"C:\a\bc" } else { "/a/bc" }),
        ));
    }

    // ── canonicalize_best_effort ────────────────────────────────────

    #[test]
    fn canonicalize_best_effort_existing_path() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let result = canonicalize_best_effort(dir.path());
        let expected = std::fs::canonicalize(dir.path()).expect("canonicalize");
        assert_eq!(result, expected);
    }

    #[test]
    fn canonicalize_best_effort_nonexistent_returns_normalized() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let nonexistent = dir.path().join("does_not_exist");
        let result = canonicalize_best_effort(&nonexistent);
        let canon_dir = std::fs::canonicalize(dir.path()).expect("canonicalize");
        assert_eq!(result, canon_dir.join("does_not_exist"));
    }

    #[test]
    fn canonicalize_best_effort_totally_bogus_returns_original() {
        let path = Path::new("relative/path/no/root");
        let result = canonicalize_best_effort(path);
        assert_eq!(result, path.to_path_buf());
    }

    // ── is_descendant_or_equal with symlinks ──────────────────────

    #[cfg(unix)]
    #[test]
    fn descendant_or_equal_through_symlink() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir(&real).expect("create real dir");
        std::os::unix::fs::symlink(&real, &link).expect("create symlink");
        let child = link.join("child");
        std::fs::create_dir(&child).expect("create child dir");
        // child is under link which points to real -- should be descendant of real
        assert!(is_descendant_or_equal(&real, &child));
    }

    // ── canonicalize_existing_prefix ────────────────────────────────

    #[test]
    fn canonicalize_existing_prefix_full_path_exists() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let result = canonicalize_existing_prefix(dir.path()).unwrap();
        let expected = std::fs::canonicalize(dir.path()).expect("canonicalize");
        assert_eq!(result, expected);
    }

    #[test]
    fn canonicalize_existing_prefix_partial_path() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let nonexistent = dir.path().join("a").join("b").join("c");
        let result = canonicalize_existing_prefix(&nonexistent).unwrap();
        let canon_dir = std::fs::canonicalize(dir.path()).expect("canonicalize");
        // Should be canon_dir/a/b/c
        assert_eq!(result, canon_dir.join("a").join("b").join("c"));
    }

    #[test]
    fn canonicalize_existing_prefix_no_ancestor_exists() {
        // When no component exists, falls back to normalize_lexical
        #[cfg(target_os = "windows")]
        let path = Path::new(r"Z:\surely\nonexistent\deep\path");
        #[cfg(not(target_os = "windows"))]
        let path = Path::new("/surely_nonexistent_root_abc123/deep/path");

        let result = canonicalize_existing_prefix(path).unwrap();
        let expected = normalize_lexical(path).unwrap();
        assert_eq!(result, expected);
    }

    // ── normalize_lexical ───────────────────────────────────────────

    #[test]
    fn normalize_lexical_resolves_dot() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\.\b");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/./b");

        let result = normalize_lexical(input).unwrap();

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

        let result = normalize_lexical(input).unwrap();

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

        let result = normalize_lexical(input).unwrap();

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\b\c"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn normalize_lexical_excess_dotdot_returns_error() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\..\..\etc\passwd");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/../../etc/passwd");

        let err = normalize_lexical(input).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("escapes root"), "unexpected error: {msg}");
    }

    #[test]
    fn normalize_lexical_relative_path_returns_error() {
        let input = Path::new("relative/path");
        let err = normalize_lexical(input).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("absolute"), "unexpected error: {msg}");
    }

    #[test]
    fn normalize_lexical_root_only() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/");

        let result = normalize_lexical(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn normalize_lexical_dotdot_at_boundary_succeeds() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\..\b");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/../b");

        let result = normalize_lexical(input).unwrap();

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\b"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/b"));
    }

    #[test]
    fn normalize_lexical_multi_level_dotdot() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\b\c\..\..\d");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/b/c/../../d");

        let result = normalize_lexical(input).unwrap();

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\d"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/d"));
    }

    #[test]
    fn normalize_lexical_trailing_dot() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\b\.");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/b/.");

        let result = normalize_lexical(input).unwrap();

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\b"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/b"));
    }

    #[test]
    fn normalize_lexical_trailing_dotdot() {
        #[cfg(target_os = "windows")]
        let input = Path::new(r"C:\a\b\c\..");
        #[cfg(not(target_os = "windows"))]
        let input = Path::new("/a/b/c/..");

        let result = normalize_lexical(input).unwrap();

        #[cfg(target_os = "windows")]
        assert_eq!(result, PathBuf::from(r"C:\a\b"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, PathBuf::from("/a/b"));
    }
}
