use std::path::{Path, PathBuf};

use crate::policy::{ResourceLimits, SandboxPolicy};

/// Ergonomic builder for [`SandboxPolicy`] that handles path canonicalization,
/// overlap deduction, and platform-specific defaults.
///
/// Paths are canonicalized on insert. Non-existent paths are silently skipped.
/// If a narrower path is already covered by a broader entry in the same or a
/// higher-privilege set, the narrower entry is not added.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicyBuilder {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    exec_paths: Vec<PathBuf>,
    deny_paths: Vec<PathBuf>,
    allow_network: bool,
    limits: ResourceLimits,
}

/// Returns true if `path` is already covered by any entry in `set` — either
/// an exact match or a parent directory.
fn covered_by(path: &Path, set: &[PathBuf]) -> bool {
    set.iter().any(|existing| path.starts_with(existing))
}

/// Remove entries from `set` that are subsumed by `parent` (children and
/// exact matches).
fn remove_covered_by(set: &mut Vec<PathBuf>, parent: &Path) {
    set.retain(|existing| !existing.starts_with(parent));
}

impl SandboxPolicyBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a read-only path. Canonicalized on insert; silently skipped if
    /// non-existent or already covered by a write or read entry.
    #[must_use]
    pub fn read_path(mut self, path: impl AsRef<Path>) -> Self {
        if let Ok(canon) = std::fs::canonicalize(path.as_ref()) {
            // Write is a superset of read — if write already covers this, skip.
            if !covered_by(&canon, &self.write_paths) && !covered_by(&canon, &self.read_paths) {
                remove_covered_by(&mut self.read_paths, &canon);
                self.read_paths.push(canon);
            }
        }
        self
    }

    /// Add a read-write path. Canonicalized on insert; silently skipped if
    /// non-existent or already covered by an existing write entry.
    #[must_use]
    pub fn write_path(mut self, path: impl AsRef<Path>) -> Self {
        if let Ok(canon) = std::fs::canonicalize(path.as_ref()) {
            if !covered_by(&canon, &self.write_paths) {
                // A write path supersedes any read entries it covers.
                remove_covered_by(&mut self.read_paths, &canon);
                remove_covered_by(&mut self.write_paths, &canon);
                self.write_paths.push(canon);
            }
        }
        self
    }

    /// Add an executable path. Canonicalized on insert; silently skipped if
    /// non-existent or already covered by an existing exec entry.
    #[must_use]
    pub fn exec_path(mut self, path: impl AsRef<Path>) -> Self {
        if let Ok(canon) = std::fs::canonicalize(path.as_ref()) {
            if !covered_by(&canon, &self.exec_paths) {
                remove_covered_by(&mut self.exec_paths, &canon);
                self.exec_paths.push(canon);
            }
        }
        self
    }

    /// Add a deny path. Canonicalized on insert; silently skipped if
    /// non-existent. No deduplication against grant paths — the deny is
    /// intentional and preserved.
    #[must_use]
    pub fn deny_path(mut self, path: impl AsRef<Path>) -> Self {
        if let Ok(canon) = std::fs::canonicalize(path.as_ref()) {
            if !self.deny_paths.contains(&canon) {
                self.deny_paths.push(canon);
            }
        }
        self
    }

    /// Add multiple deny paths. Convenience wrapper around [`deny_path`](Self::deny_path).
    #[must_use]
    pub fn deny_paths(mut self, paths: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        for p in paths {
            self = self.deny_path(p);
        }
        self
    }

    /// Add the platform temp directory to write paths.
    #[must_use]
    pub fn include_temp_dirs(self) -> Self {
        self.write_path(std::env::temp_dir())
    }

    /// Add standard platform executable directories to exec paths.
    #[must_use]
    pub fn include_platform_exec_paths(mut self) -> Self {
        for p in platform_exec_paths() {
            self = self.exec_path(p);
        }
        self
    }

    /// Add standard platform library/include directories to read paths.
    #[must_use]
    pub fn include_platform_lib_paths(mut self) -> Self {
        for p in platform_lib_paths() {
            self = self.read_path(p);
        }
        self
    }

    /// Set whether outbound network access is allowed.
    #[must_use]
    pub const fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }

    /// Set the maximum memory limit in bytes.
    #[must_use]
    pub const fn max_memory_bytes(mut self, bytes: u64) -> Self {
        self.limits.max_memory_bytes = Some(bytes);
        self
    }

    /// Set the maximum number of child processes.
    #[must_use]
    pub const fn max_processes(mut self, n: u32) -> Self {
        self.limits.max_processes = Some(n);
        self
    }

    /// Set the maximum CPU time in seconds.
    #[must_use]
    pub const fn max_cpu_seconds(mut self, seconds: u64) -> Self {
        self.limits.max_cpu_seconds = Some(seconds);
        self
    }

    /// Consume the builder and produce a validated [`SandboxPolicy`].
    ///
    /// Returns [`SandboxError::InvalidPolicy`] if the resulting policy fails
    /// validation (e.g. no paths at all, or zero resource limits).
    pub fn build(self) -> crate::Result<SandboxPolicy> {
        let policy = SandboxPolicy::new(
            self.read_paths,
            self.write_paths,
            self.exec_paths,
            self.deny_paths,
            self.allow_network,
            self.limits,
        );
        policy.validate()?;
        Ok(policy)
    }
}

/// Platform executable directories (shells, common tools).
fn platform_exec_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut paths = Vec::new();
        if let Ok(sysroot) = std::env::var("SYSTEMROOT") {
            paths.push(PathBuf::from(format!("{sysroot}\\System32")));
        }
        paths
    }

    #[cfg(unix)]
    {
        vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
            PathBuf::from("/usr/sbin"),
            PathBuf::from("/sbin"),
            PathBuf::from("/usr/local/bin"),
        ]
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Vec::new()
    }
}

/// Platform library/include directories needed by compilers and build tools.
fn platform_lib_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut paths = Vec::new();
        if let Ok(prog) = std::env::var("ProgramFiles") {
            paths.push(PathBuf::from(prog));
        }
        if let Ok(prog86) = std::env::var("ProgramFiles(x86)") {
            paths.push(PathBuf::from(prog86));
        }
        paths
    }

    #[cfg(unix)]
    {
        let paths = vec![
            PathBuf::from("/usr/lib"),
            PathBuf::from("/usr/include"),
            PathBuf::from("/usr/local/lib"),
            PathBuf::from("/usr/local/include"),
            #[cfg(target_os = "macos")]
            PathBuf::from("/Library/Developer"),
        ];
        paths
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Vec::new()
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

    #[test]
    fn basic_build() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
        assert!(policy.write_paths().is_empty());
        assert!(!policy.allow_network());
    }

    #[test]
    fn nonexistent_path_silently_skipped() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .read_path("/surely/does/not/exist/xyz")
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
    }

    #[test]
    fn write_parent_removes_read_child() {
        let tmp = make_temp_dir();
        let child = tmp.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&child)
            .write_path(tmp.path())
            .build()
            .expect("build should succeed");

        // The read child should have been deduplicated away.
        assert!(policy.read_paths().is_empty());
        assert_eq!(policy.write_paths().len(), 1);
    }

    #[test]
    fn read_covered_by_write_not_added() {
        let tmp = make_temp_dir();
        let child = tmp.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");

        let policy = SandboxPolicyBuilder::new()
            .write_path(tmp.path())
            .read_path(&child)
            .build()
            .expect("build should succeed");

        assert!(policy.read_paths().is_empty());
        assert_eq!(policy.write_paths().len(), 1);
    }

    #[test]
    fn duplicate_read_path_deduplicated() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .read_path(tmp.path())
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
    }

    #[test]
    fn duplicate_write_path_deduplicated() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .write_path(tmp.path())
            .write_path(tmp.path())
            .build()
            .expect("build should succeed");
        assert_eq!(policy.write_paths().len(), 1);
    }

    #[test]
    fn read_parent_then_read_child_deduplicated() {
        let tmp = make_temp_dir();
        let child = tmp.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .read_path(&child)
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
    }

    #[test]
    fn read_child_then_read_parent_collapses() {
        let tmp = make_temp_dir();
        let child = tmp.path().join("sub");
        std::fs::create_dir(&child).expect("create subdir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&child)
            .read_path(tmp.path())
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
    }

    #[test]
    fn allow_network_flag() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .allow_network(true)
            .build()
            .expect("build should succeed");
        assert!(policy.allow_network());
    }

    #[test]
    fn resource_limits_set() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .max_memory_bytes(1024 * 1024)
            .max_processes(10)
            .max_cpu_seconds(60)
            .build()
            .expect("build should succeed");
        assert_eq!(policy.limits().max_memory_bytes, Some(1024 * 1024));
        assert_eq!(policy.limits().max_processes, Some(10));
        assert_eq!(policy.limits().max_cpu_seconds, Some(60));
    }

    #[test]
    fn zero_resource_limit_rejected() {
        let tmp = make_temp_dir();
        let result = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .max_memory_bytes(0)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn empty_builder_rejected() {
        let result = SandboxPolicyBuilder::new().build();
        assert!(result.is_err());
    }

    #[test]
    fn include_temp_dirs_adds_write_path() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .include_temp_dirs()
            .build()
            .expect("build should succeed");

        // Temp dir should appear in write_paths (canonicalized).
        let temp_canon =
            std::fs::canonicalize(std::env::temp_dir()).expect("temp dir should exist");
        assert!(
            policy.write_paths().iter().any(|p| p == &temp_canon),
            "write_paths should contain temp dir"
        );
    }

    #[test]
    fn exec_path_deduplication() {
        let tmp = make_temp_dir();
        let read_dir = tmp.path().join("src");
        let exec_dir = tmp.path().join("bin");
        std::fs::create_dir(&read_dir).expect("create src dir");
        std::fs::create_dir(&exec_dir).expect("create bin dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&read_dir)
            .exec_path(&exec_dir)
            .exec_path(&exec_dir)
            .build()
            .expect("build should succeed");
        assert_eq!(policy.exec_paths().len(), 1);
    }

    #[test]
    fn built_policy_passes_validate() {
        let tmp = make_temp_dir();
        let read_dir = tmp.path().join("read");
        let write_dir = tmp.path().join("write");
        std::fs::create_dir(&read_dir).expect("create read dir");
        std::fs::create_dir(&write_dir).expect("create write dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&read_dir)
            .write_path(&write_dir)
            .allow_network(false)
            .max_memory_bytes(512 * 1024 * 1024)
            .build()
            .expect("build should succeed");

        // The produced policy should pass validate() independently.
        policy.validate().expect("validate should pass");
    }
}
