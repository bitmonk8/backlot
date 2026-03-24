use std::path::PathBuf;

use crate::error::SandboxError;

/// Defines the filesystem, network, and resource constraints for a sandboxed
/// process.
///
/// Paths in `read_paths`, `write_paths`, `exec_paths`, and `deny_paths` must
/// exist. Grant paths must not overlap with each other, except that a write
/// child under a read parent is allowed (directional overlap for elevated
/// subdirectory permissions). Each deny path must be a strict child of a grant
/// path, and no grant path may be nested under a deny path.
/// Call [`SandboxPolicy::validate`] (or let [`spawn`](crate::spawn) call it)
/// to check these constraints.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    exec_paths: Vec<PathBuf>,
    deny_paths: Vec<PathBuf>,
    allow_network: bool,
    limits: ResourceLimits,
    pub(crate) sentinel_dir: Option<PathBuf>,
}

impl SandboxPolicy {
    /// Direct constructor. Prefer [`SandboxPolicyBuilder`](crate::SandboxPolicyBuilder) for
    /// deduplication and canonicalization.
    pub const fn new(
        read_paths: Vec<PathBuf>,
        write_paths: Vec<PathBuf>,
        exec_paths: Vec<PathBuf>,
        deny_paths: Vec<PathBuf>,
        allow_network: bool,
        limits: ResourceLimits,
    ) -> Self {
        Self {
            read_paths,
            write_paths,
            exec_paths,
            deny_paths,
            allow_network,
            limits,
            sentinel_dir: None,
        }
    }

    /// Paths the child can read (recursive).
    pub fn read_paths(&self) -> &[PathBuf] {
        &self.read_paths
    }

    /// Paths the child can read and write (recursive).
    pub fn write_paths(&self) -> &[PathBuf] {
        &self.write_paths
    }

    /// Paths the child can execute from (recursive).
    pub fn exec_paths(&self) -> &[PathBuf] {
        &self.exec_paths
    }

    /// Subtrees denied all access, overriding any grants.
    pub fn deny_paths(&self) -> &[PathBuf] {
        &self.deny_paths
    }

    /// Whether outbound network access is allowed.
    pub const fn allow_network(&self) -> bool {
        self.allow_network
    }

    /// Resource limits.
    pub const fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    /// Directory for sentinel files (Windows). When `None`, uses the
    /// system temp directory.
    pub fn sentinel_dir(&self) -> Option<&std::path::Path> {
        self.sentinel_dir.as_deref()
    }
}

/// Canonicalize a path, mapping IO errors to `InvalidPolicy` with the list name.
fn canonicalize_for_validation(
    path: &std::path::Path,
    which: &str,
) -> Result<PathBuf, SandboxError> {
    std::fs::canonicalize(path).map_err(|e| {
        SandboxError::InvalidPolicy(format!(
            "path does not exist or is inaccessible in {which}: {} ({e})",
            path.display()
        ))
    })
}

/// Controls whether `check_cross_overlap` rejects both nesting directions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OverlapMode {
    /// Reject nesting in both directions (a-under-b and b-under-a).
    Symmetric,
    /// Allow b-children under a-parents (elevated subdirectory under
    /// lower-privilege parent, e.g. write child under read parent).
    AllowChildUnderParent,
}

/// Check for parent/child overlaps between two named sets of canonicalized paths.
fn check_cross_overlap(
    a_paths: &[PathBuf],
    a_name: &str,
    b_paths: &[PathBuf],
    b_name: &str,
    mode: OverlapMode,
) -> Result<(), SandboxError> {
    for a in a_paths {
        for b in b_paths {
            if a == b {
                return Err(SandboxError::InvalidPolicy(format!(
                    "path appears in both {a_name} and {b_name}: {}",
                    a.display()
                )));
            }
            // b parent of a: always rejected (a is redundant or lower-priv
            // child under higher-priv parent).
            if crate::path_util::is_strict_parent_of(b, a) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "parent/child overlap between {b_name} and {a_name}: {} contains {}",
                    b.display(),
                    a.display()
                )));
            }
            // a parent of b: rejected unless directional mode allows it
            // (elevated subdirectory under lower-privilege parent).
            if mode == OverlapMode::Symmetric && crate::path_util::is_strict_parent_of(a, b) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "parent/child overlap between {a_name} and {b_name}: {} contains {}",
                    a.display(),
                    b.display()
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
            if crate::path_util::is_strict_parent_of(a, b)
                || crate::path_util::is_strict_parent_of(b, a)
            {
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

/// Verify that each deny path is a strict child of at least one grant path,
/// and that no grant path is a child of a deny path (unreachable grant).
fn validate_deny_paths(
    deny_paths: &[PathBuf],
    read_paths: &[PathBuf],
    write_paths: &[PathBuf],
    exec_paths: &[PathBuf],
) -> Result<(), SandboxError> {
    let all_grants: Vec<&PathBuf> = read_paths
        .iter()
        .chain(write_paths.iter())
        .chain(exec_paths.iter())
        .collect();

    for deny in deny_paths {
        let covered = all_grants
            .iter()
            .any(|grant| crate::path_util::is_strict_parent_of(grant, deny));
        if !covered {
            return Err(SandboxError::InvalidPolicy(format!(
                "deny path is not a strict child of any grant path: {}",
                deny.display()
            )));
        }
    }

    // Reject grant paths nested under deny paths — they would be unreachable.
    for grant in &all_grants {
        for deny in deny_paths {
            if crate::path_util::is_strict_parent_of(deny, grant) {
                return Err(SandboxError::InvalidPolicy(format!(
                    "grant path {} is under deny path {} and would be unreachable",
                    grant.display(),
                    deny.display()
                )));
            }
        }
    }

    Ok(())
}

/// Canonicalize a list of paths, collecting errors into `errors` and returning
/// successfully canonicalized paths.
fn canonicalize_collect(paths: &[PathBuf], label: &str, errors: &mut Vec<String>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for p in paths {
        match canonicalize_for_validation(p, label) {
            Ok(c) => result.push(c),
            Err(SandboxError::InvalidPolicy(msg)) => errors.push(msg),
            Err(e) => errors.push(e.to_string()),
        }
    }
    result
}

/// If `result` is an error, push the message into `errors`.
fn collect_validation_error(result: Result<(), SandboxError>, errors: &mut Vec<String>) {
    match result {
        Ok(()) => {}
        Err(SandboxError::InvalidPolicy(msg)) => errors.push(msg),
        Err(other) => errors.push(other.to_string()),
    }
}

// ── Path queries and validation ──────────────────────────────────────

impl SandboxPolicy {
    /// Returns the union of `read_paths`, `write_paths`, `exec_paths`, and `deny_paths`.
    pub fn all_paths(&self) -> Vec<&std::path::Path> {
        self.read_paths
            .iter()
            .chain(self.write_paths.iter())
            .chain(self.exec_paths.iter())
            .chain(self.deny_paths.iter())
            .map(PathBuf::as_path)
            .collect()
    }

    /// Returns the union of `read_paths`, `write_paths`, and `exec_paths` (grant paths only).
    /// Excludes `deny_paths`. Used by `env_check` for PATH reachability validation.
    pub fn grant_paths(&self) -> Vec<&std::path::Path> {
        self.read_paths
            .iter()
            .chain(self.write_paths.iter())
            .chain(self.exec_paths.iter())
            .map(PathBuf::as_path)
            .collect()
    }

    /// Validate the policy before applying it.
    ///
    /// Returns [`SandboxError::InvalidPolicy`] containing all validation
    /// errors (not just the first) if any path does not exist, if grant paths
    /// overlap across or within sets, if deny paths are not strict children of
    /// grant paths, if a grant path is nested under a deny path, or if
    /// resource limits are zero.
    /// Called automatically by [`spawn()`](crate::spawn).
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidPolicy`] with a semicolon-delimited
    /// description of all validation failures found.
    pub fn validate(&self) -> Result<(), SandboxError> {
        let mut errors: Vec<String> = Vec::new();

        if self.read_paths.is_empty() && self.write_paths.is_empty() && self.exec_paths.is_empty() {
            errors.push("policy must specify at least one path".into());
        }

        // Re-canonicalize even though the builder may have already canonicalized.
        // This catches paths constructed via `SandboxPolicy::new()` directly and
        // validates that previously-canonical paths still exist on disk.
        // Collect canonicalization errors but continue with successfully
        // canonicalized paths so subsequent checks can still run.
        // Paths that fail canonicalization are intentionally excluded from overlap
        // checks — their non-existence is already reported as an error above.
        let read_canon = canonicalize_collect(&self.read_paths, "read_paths", &mut errors);
        let write_canon = canonicalize_collect(&self.write_paths, "write_paths", &mut errors);
        let exec_canon = canonicalize_collect(&self.exec_paths, "exec_paths", &mut errors);
        let deny_canon = canonicalize_collect(&self.deny_paths, "deny_paths", &mut errors);

        // Check for intra-set overlaps.
        collect_validation_error(check_intra_overlap(&read_canon, "read_paths"), &mut errors);
        collect_validation_error(
            check_intra_overlap(&write_canon, "write_paths"),
            &mut errors,
        );
        collect_validation_error(check_intra_overlap(&exec_canon, "exec_paths"), &mut errors);
        collect_validation_error(check_intra_overlap(&deny_canon, "deny_paths"), &mut errors);

        // Check for cross-set overlaps (exact match + parent/child).
        //
        // A write child under a read parent is allowed: it grants elevated
        // permissions to a specific subdirectory while the parent remains
        // read-only. The reverse (read child under write parent) is redundant
        // and rejected.
        collect_validation_error(
            check_cross_overlap(
                &read_canon,
                "read_paths",
                &write_canon,
                "write_paths",
                OverlapMode::AllowChildUnderParent,
            ),
            &mut errors,
        );
        collect_validation_error(
            check_cross_overlap(
                &read_canon,
                "read_paths",
                &exec_canon,
                "exec_paths",
                OverlapMode::Symmetric,
            ),
            &mut errors,
        );
        collect_validation_error(
            check_cross_overlap(
                &write_canon,
                "write_paths",
                &exec_canon,
                "exec_paths",
                OverlapMode::Symmetric,
            ),
            &mut errors,
        );

        // Each deny path must be a strict child of at least one grant path.
        // Exact matches are rejected — callers should remove the grant instead.
        collect_validation_error(
            validate_deny_paths(&deny_canon, &read_canon, &write_canon, &exec_canon),
            &mut errors,
        );

        collect_validation_error(self.limits.validate(), &mut errors);

        if errors.is_empty() {
            Ok(())
        } else {
            Err(SandboxError::InvalidPolicy(errors.join("; ")))
        }
    }
}

/// Optional resource constraints (memory, processes, CPU time) for the
/// sandboxed process. All fields default to `None` (no limit).
///
/// Platform enforcement varies:
/// - **Windows**: all limits enforced via Job Objects.
/// - **macOS**: memory and process limits via `setrlimit`; CPU time via `RLIMIT_CPU`.
/// - **Linux**: memory and process limits via cgroupv2; CPU time is **not enforced**
///   (cgroupv2 `cpu.max` controls bandwidth/rate, not total accumulated time).
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    /// Maximum memory in bytes. None = no limit.
    ///
    /// On macOS, this uses `setrlimit(RLIMIT_AS)` which limits virtual address
    /// space. `setrlimit` returns `EINVAL` if the limit is below the forked
    /// child's inherited VM size (often >4 GB on Apple Silicon due to the dyld
    /// shared cache). `spawn()` returns `SandboxError::Setup` in this case.
    /// Linux (cgroups v2) and Windows (job objects) are not affected.
    pub max_memory_bytes: Option<u64>,
    /// Maximum number of child processes. None = no limit.
    pub max_processes: Option<u32>,
    /// Maximum CPU time in seconds. None = no limit.
    ///
    /// Enforced on Windows (Job Objects) and macOS (`RLIMIT_CPU`).
    /// **Not enforced on Linux** — cgroupv2 `cpu.max` controls CPU bandwidth
    /// (rate limiting), not total accumulated CPU time.
    pub max_cpu_seconds: Option<u64>,
}

impl ResourceLimits {
    /// Returns `true` if any resource limit is set.
    ///
    /// Uses destructuring so adding a field to `ResourceLimits` without
    /// updating this function produces a compile error.
    pub const fn has_any(&self) -> bool {
        let Self {
            max_memory_bytes,
            max_processes,
            max_cpu_seconds,
        } = self;
        max_memory_bytes.is_some() || max_processes.is_some() || max_cpu_seconds.is_some()
    }

    fn validate(&self) -> Result<(), SandboxError> {
        let mut msgs: Vec<&str> = Vec::new();
        if self.max_memory_bytes == Some(0) {
            msgs.push("max_memory_bytes must not be zero");
        }
        if self.max_processes == Some(0) {
            msgs.push("max_processes must not be zero");
        }
        if self.max_cpu_seconds == Some(0) {
            msgs.push("max_cpu_seconds must not be zero");
        }
        if msgs.is_empty() {
            Ok(())
        } else {
            Err(SandboxError::InvalidPolicy(msgs.join("; ")))
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_temp_dir() -> TempDir {
        let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
        TempDir::new_in(test_tmp).expect("failed to create temp dir")
    }

    fn valid_policy(path: PathBuf) -> SandboxPolicy {
        SandboxPolicy {
            read_paths: vec![path],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_memory_bytes: Some(1024 * 1024),
                max_processes: Some(10),
                max_cpu_seconds: Some(60),
            },
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_memory_bytes: Some(0),
                ..ResourceLimits::default()
            },
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_processes: Some(0),
                ..ResourceLimits::default()
            },
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_cpu_seconds: Some(0),
                ..ResourceLimits::default()
            },
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate"),
            "error should mention duplicate: {msg}"
        );
    }

    #[test]
    fn exec_parent_read_child_overlap_rejected() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let child = tmp.path().join("bin");
        std::fs::create_dir(&child).expect("create subdir");

        let policy = SandboxPolicy {
            read_paths: vec![child],
            write_paths: Vec::new(),
            exec_paths: vec![parent],
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("overlap"),
            "error should mention overlap: {msg}"
        );
    }

    #[test]
    fn exec_parent_write_child_overlap_rejected() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let child = tmp.path().join("data");
        std::fs::create_dir(&child).expect("create subdir");

        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: vec![child],
            exec_paths: vec![parent],
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
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
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate"),
            "error should mention duplicate: {msg}"
        );
    }

    #[test]
    fn deny_path_valid_strict_child_of_grant() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let denied = tmp.path().join("secrets");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicy {
            read_paths: vec![parent],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: vec![denied],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        policy.validate().expect("valid policy with deny path");
    }

    #[test]
    fn deny_path_not_covered_by_any_grant_rejected() {
        let tmp = make_temp_dir();
        let denied = tmp.path().to_path_buf();

        let other = make_temp_dir();
        let grant = other.path().to_path_buf();

        let policy = SandboxPolicy {
            read_paths: vec![grant],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: vec![denied],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("deny path is not a strict child"),
            "error should mention deny coverage: {msg}"
        );
    }

    #[test]
    fn deny_path_exact_match_with_grant_rejected() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();

        let policy = SandboxPolicy {
            read_paths: vec![p.clone()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: vec![p],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("deny path is not a strict child"),
            "exact match should be rejected: {msg}"
        );
    }

    #[test]
    fn deny_paths_intra_overlap_rejected() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let deny_parent = tmp.path().join("a");
        let deny_child = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&deny_child).expect("create nested dirs");

        let policy = SandboxPolicy {
            read_paths: vec![parent],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: vec![deny_parent, deny_child],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("overlap") || msg.contains("parent"),
            "nested deny paths should be rejected: {msg}"
        );
    }

    #[test]
    fn grant_under_deny_rejected() {
        let tmp = make_temp_dir();
        let grant_parent = tmp.path().to_path_buf();
        let denied = tmp.path().join("denied");
        let grant_under_deny = tmp.path().join("denied").join("exception");
        std::fs::create_dir_all(&grant_under_deny).expect("create dirs");

        let policy = SandboxPolicy {
            read_paths: vec![grant_parent],
            write_paths: vec![grant_under_deny],
            exec_paths: Vec::new(),
            deny_paths: vec![denied],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unreachable"),
            "grant under deny should be rejected: {msg}"
        );
    }

    #[test]
    fn all_paths_returns_all_four_types() {
        let tmp = make_temp_dir();
        let read = tmp.path().join("r");
        let write = tmp.path().join("w");
        let exec = tmp.path().join("e");
        let deny = tmp.path().join("d");
        for p in [&read, &write, &exec, &deny] {
            std::fs::create_dir(p).expect("create dir");
        }

        let policy = SandboxPolicy {
            read_paths: vec![read.clone()],
            write_paths: vec![write.clone()],
            exec_paths: vec![exec.clone()],
            deny_paths: vec![deny.clone()],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };

        let all = policy.all_paths();
        assert_eq!(all.len(), 4);
        assert!(all.contains(&read.as_path()));
        assert!(all.contains(&write.as_path()));
        assert!(all.contains(&exec.as_path()));
        assert!(all.contains(&deny.as_path()));
    }

    #[test]
    fn grant_paths_excludes_deny() {
        let tmp = make_temp_dir();
        let read = tmp.path().join("r");
        let deny = tmp.path().join("d");
        for p in [&read, &deny] {
            std::fs::create_dir(p).expect("create dir");
        }

        let policy = SandboxPolicy {
            read_paths: vec![read.clone()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: vec![deny.clone()],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };

        let grant = policy.grant_paths();
        assert_eq!(grant.len(), 1);
        assert!(grant.contains(&read.as_path()));
        assert!(!grant.contains(&deny.as_path()));
    }

    // ── has_any() ──────────────────────────────────────────────────

    #[test]
    fn has_any_returns_false_for_default() {
        let limits = ResourceLimits::default();
        assert!(!limits.has_any());
    }

    #[test]
    fn has_any_returns_true_for_memory() {
        let limits = ResourceLimits {
            max_memory_bytes: Some(1024),
            ..ResourceLimits::default()
        };
        assert!(limits.has_any());
    }

    #[test]
    fn has_any_returns_true_for_processes() {
        let limits = ResourceLimits {
            max_processes: Some(5),
            ..ResourceLimits::default()
        };
        assert!(limits.has_any());
    }

    #[test]
    fn has_any_returns_true_for_cpu() {
        let limits = ResourceLimits {
            max_cpu_seconds: Some(30),
            ..ResourceLimits::default()
        };
        assert!(limits.has_any());
    }

    #[test]
    fn has_any_returns_true_for_all_set() {
        let limits = ResourceLimits {
            max_memory_bytes: Some(1024),
            max_processes: Some(5),
            max_cpu_seconds: Some(30),
        };
        assert!(limits.has_any());
    }

    // ── Strengthened error message assertions ─────────────────────

    #[test]
    fn empty_policy_error_mentions_at_least_one_path() {
        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("at least one path"),
            "error should mention missing paths: {msg}"
        );
    }

    #[test]
    fn conflicting_paths_error_mentions_both_sets() {
        let tmp = make_temp_dir();
        let p = tmp.path().to_path_buf();
        let policy = SandboxPolicy {
            read_paths: vec![p.clone()],
            write_paths: vec![p],
            exec_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("read_paths") && msg.contains("write_paths"),
            "error should name both conflicting sets: {msg}"
        );
    }

    #[test]
    fn zero_memory_error_mentions_field_name() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_memory_bytes: Some(0),
                ..ResourceLimits::default()
            },
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_memory_bytes"),
            "error should mention field name: {msg}"
        );
    }

    // ── validate_deny_paths with write/exec grants ────────────────

    #[test]
    fn deny_path_covered_by_write_grant() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let denied = tmp.path().join("secrets");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: vec![parent],
            exec_paths: Vec::new(),
            deny_paths: vec![denied],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        policy
            .validate()
            .expect("deny path under write grant should be valid");
    }

    #[test]
    fn deny_path_covered_by_exec_grant() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let denied = tmp.path().join("untrusted");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            exec_paths: vec![parent],
            deny_paths: vec![denied],
            allow_network: false,
            limits: ResourceLimits::default(),
            sentinel_dir: None,
        };
        policy
            .validate()
            .expect("deny path under exec grant should be valid");
    }

    #[test]
    fn validate_reports_multiple_errors() {
        let policy = SandboxPolicy {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            deny_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits {
                max_memory_bytes: Some(0),
                max_processes: Some(0),
                ..ResourceLimits::default()
            },
            sentinel_dir: None,
        };
        let err = policy.validate().unwrap_err();
        let msg = err.to_string();
        // Should contain both "at least one path" and a resource limit error.
        assert!(
            msg.contains("at least one path"),
            "should report empty policy: {msg}"
        );
        assert!(
            msg.contains("max_memory_bytes") && msg.contains("max_processes"),
            "should report resource limit errors: {msg}"
        );
    }
}
