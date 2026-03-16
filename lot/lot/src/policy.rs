use std::path::PathBuf;

use crate::error::SandboxError;

/// Defines the filesystem, network, and resource constraints for a sandboxed
/// process.
///
/// Paths in `read_paths`, `write_paths`, and `exec_paths` must exist and must
/// not overlap with each other. Call [`SandboxPolicy::validate`] (or let
/// [`spawn`](crate::spawn) call it) to check these constraints.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Paths the child can read (recursive).
    pub read_paths: Vec<PathBuf>,
    /// Paths the child can read and write (recursive).
    pub write_paths: Vec<PathBuf>,
    /// Paths the child can execute from (recursive).
    pub exec_paths: Vec<PathBuf>,
    /// Allow outbound network access.
    pub allow_network: bool,
    /// Resource limits.
    pub limits: ResourceLimits,
}

/// Canonicalize a path, mapping IO errors to `InvalidPolicy` with the list name.
fn canon(path: &std::path::Path, which: &str) -> Result<PathBuf, SandboxError> {
    std::fs::canonicalize(path).map_err(|e| {
        SandboxError::InvalidPolicy(format!(
            "path does not exist or is inaccessible in {which}: {} ({e})",
            path.display()
        ))
    })
}

/// Returns true if `parent` is a strict prefix of `child` (directory ancestry).
fn is_parent_of(parent: &std::path::Path, child: &std::path::Path) -> bool {
    // `starts_with` on Path checks component-wise, so /tmp won't match /tmpfoo.
    child.starts_with(parent) && child != parent
}

/// Check for parent/child overlaps between two named sets of canonicalized paths.
fn check_cross_overlap(
    a_paths: &[PathBuf],
    a_name: &str,
    b_paths: &[PathBuf],
    b_name: &str,
) -> Result<(), SandboxError> {
    for a in a_paths {
        for b in b_paths {
            if a == b {
                return Err(SandboxError::InvalidPolicy(format!(
                    "path appears in both {a_name} and {b_name}: {}",
                    a.display()
                )));
            }
            if is_parent_of(a, b) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "parent/child overlap between {a_name} and {b_name}: {} contains {}",
                    a.display(),
                    b.display()
                )));
            }
            if is_parent_of(b, a) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "parent/child overlap between {b_name} and {a_name}: {} contains {}",
                    b.display(),
                    a.display()
                )));
            }
        }
    }
    Ok(())
}

/// Like [`check_cross_overlap`] but allows `b` (higher-privilege) children
/// under `a` (lower-privilege) parents.  Rejects exact duplicates, `a` children
/// under `b` parents (redundant), and `b` parents over `a` children.
fn check_cross_overlap_directional(
    a_paths: &[PathBuf],
    a_name: &str,
    b_paths: &[PathBuf],
    b_name: &str,
) -> Result<(), SandboxError> {
    for a in a_paths {
        for b in b_paths {
            if a == b {
                return Err(SandboxError::InvalidPolicy(format!(
                    "path appears in both {a_name} and {b_name}: {}",
                    a.display()
                )));
            }
            // b child under a parent is allowed (elevated subdirectory).
            // a child under b parent is redundant (b already covers a).
            if is_parent_of(b, a) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "parent/child overlap between {b_name} and {a_name}: {} contains {}",
                    b.display(),
                    a.display()
                )));
            }
        }
    }
    Ok(())
}

/// Check for parent/child overlaps within a single named set.
fn check_intra_overlap(paths: &[PathBuf], name: &str) -> Result<(), SandboxError> {
    for (i, a) in paths.iter().enumerate() {
        for b in &paths[i + 1..] {
            if a == b {
                return Err(SandboxError::InvalidPolicy(format!(
                    "duplicate path in {name}: {}",
                    a.display()
                )));
            }
            if is_parent_of(a, b) || is_parent_of(b, a) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "parent/child overlap within {name}: {} and {}",
                    a.display(),
                    b.display()
                )));
            }
        }
    }
    Ok(())
}

impl SandboxPolicy {
    /// Returns the union of `read_paths`, `write_paths`, and `exec_paths`.
    pub fn all_paths(&self) -> Vec<&std::path::Path> {
        self.read_paths
            .iter()
            .chain(self.write_paths.iter())
            .chain(self.exec_paths.iter())
            .map(PathBuf::as_path)
            .collect()
    }

    /// Validate the policy before applying it.
    ///
    /// Returns [`SandboxError::InvalidPolicy`] if any path does not exist, if
    /// paths overlap across or within sets, or if resource limits are zero.
    /// Called automatically by [`spawn()`](crate::spawn).
    pub fn validate(&self) -> Result<(), SandboxError> {
        if self.read_paths.is_empty() && self.write_paths.is_empty() && self.exec_paths.is_empty() {
            return Err(SandboxError::InvalidPolicy(
                "policy must specify at least one path".into(),
            ));
        }

        // Canonicalize all paths. This also validates existence.
        let read_canon: Vec<PathBuf> = self
            .read_paths
            .iter()
            .map(|p| canon(p, "read_paths"))
            .collect::<Result<_, _>>()?;
        let write_canon: Vec<PathBuf> = self
            .write_paths
            .iter()
            .map(|p| canon(p, "write_paths"))
            .collect::<Result<_, _>>()?;
        let exec_canon: Vec<PathBuf> = self
            .exec_paths
            .iter()
            .map(|p| canon(p, "exec_paths"))
            .collect::<Result<_, _>>()?;

        // Check for intra-set overlaps.
        check_intra_overlap(&read_canon, "read_paths")?;
        check_intra_overlap(&write_canon, "write_paths")?;
        check_intra_overlap(&exec_canon, "exec_paths")?;

        // Check for cross-set overlaps (exact match + parent/child).
        //
        // A write child under a read parent is allowed: it grants elevated
        // permissions to a specific subdirectory while the parent remains
        // read-only. The reverse (read child under write parent) is redundant
        // and rejected.
        check_cross_overlap_directional(&read_canon, "read_paths", &write_canon, "write_paths")?;
        check_cross_overlap(&read_canon, "read_paths", &exec_canon, "exec_paths")?;
        check_cross_overlap(&write_canon, "write_paths", &exec_canon, "exec_paths")?;

        self.limits.validate()?;

        Ok(())
    }
}

/// Optional resource constraints (memory, processes, CPU time) for the
/// sandboxed process. All fields default to `None` (no limit).
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    /// Maximum memory in bytes. None = no limit.
    pub max_memory_bytes: Option<u64>,
    /// Maximum number of child processes. None = no limit.
    pub max_processes: Option<u32>,
    /// Maximum CPU time in seconds. None = no limit.
    pub max_cpu_seconds: Option<u64>,
}

impl ResourceLimits {
    fn validate(&self) -> Result<(), SandboxError> {
        if self.max_memory_bytes == Some(0) {
            return Err(SandboxError::InvalidPolicy(
                "max_memory_bytes must not be zero".into(),
            ));
        }
        if self.max_processes == Some(0) {
            return Err(SandboxError::InvalidPolicy(
                "max_processes must not be zero".into(),
            ));
        }
        if self.max_cpu_seconds == Some(0) {
            return Err(SandboxError::InvalidPolicy(
                "max_cpu_seconds must not be zero".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_temp_dir() -> TempDir {
        TempDir::new().expect("failed to create temp dir")
    }

    fn valid_policy(path: PathBuf) -> SandboxPolicy {
        SandboxPolicy {
            read_paths: vec![path],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        }
    }

    #[test]
    fn valid_policy_passes() {
        let tmp = make_temp_dir();
        let policy = valid_policy(tmp.path().to_path_buf());
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn empty_policy_rejected() {
        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn nonexistent_path_rejected() {
        let policy = SandboxPolicy {
            read_paths: vec![PathBuf::from("/surely/does/not/exist/abc123")],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn nonexistent_path_error_includes_list_name() {
        let policy = SandboxPolicy {
            read_paths: vec![PathBuf::from("/surely/does/not/exist/abc123")],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("read_paths"),
            "error should name the path list: {msg}"
        );
    }

    #[test]
    fn conflicting_paths_rejected() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();
        let policy = SandboxPolicy {
            read_paths: vec![p.clone()],
            write_paths: vec![p],
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn write_child_under_read_parent_allowed() {
        // A write child under a read parent is valid: it grants elevated
        // permissions to a specific subdirectory.
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let child = tmp.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        let policy = SandboxPolicy {
            read_paths: vec![parent],
            write_paths: vec![child],
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        policy
            .validate()
            .expect("write child under read parent should be valid");
    }

    #[test]
    fn parent_child_overlap_write_read_rejected() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let child = tmp.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");
        let policy = SandboxPolicy {
            read_paths: vec![child],
            write_paths: vec![parent],
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn exec_paths_overlap_with_read_rejected() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();
        let policy = SandboxPolicy {
            read_paths: vec![p.clone()],
            write_paths: Vec::new(),
            exec_paths: vec![p],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn exec_paths_overlap_with_write_rejected() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();
        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: vec![p.clone()],
            exec_paths: vec![p],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn exec_paths_only_policy_valid() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            exec_paths: vec![tmp.path().to_path_buf()],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn valid_nonzero_resource_limits_pass() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_memory_bytes: Some(1024 * 1024),
                max_processes: Some(10),
                max_cpu_seconds: Some(60),
            },
        };
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn multiple_paths_one_nonexistent_rejected() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: vec![
                tmp.path().to_path_buf(),
                PathBuf::from("/surely/does/not/exist/abc123"),
            ],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn zero_memory_limit_rejected() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_memory_bytes: Some(0),
                ..ResourceLimits::default()
            },
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn zero_processes_limit_rejected() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_processes: Some(0),
                ..ResourceLimits::default()
            },
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn zero_cpu_limit_rejected() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_cpu_seconds: Some(0),
                ..ResourceLimits::default()
            },
        };
        let err = policy.validate().unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)));
    }

    #[test]
    fn duplicate_path_in_read_paths_rejected() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();
        let policy = SandboxPolicy {
            read_paths: vec![p.clone(), p],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate"),
            "error should mention duplicate: {msg}"
        );
    }

    #[test]
    fn exec_paths_parent_child_overlap_rejected() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let child = tmp.path().join("bin");
        std::fs::create_dir(&child).expect("create subdir");

        // exec parent contains read child
        let policy = SandboxPolicy {
            read_paths: vec![child],
            write_paths: Vec::new(),
            exec_paths: vec![parent],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("overlap"),
            "error should mention overlap: {msg}"
        );
    }

    #[test]
    fn exec_paths_intra_duplicate_rejected() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();
        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            exec_paths: vec![p.clone(), p],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate"),
            "error should mention duplicate: {msg}"
        );
    }
}
