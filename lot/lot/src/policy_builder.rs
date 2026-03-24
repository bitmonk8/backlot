use std::io;
use std::path::{Path, PathBuf};

use crate::error::SandboxError;
use crate::policy::{ResourceLimits, SandboxPolicy};

/// Ergonomic builder for [`SandboxPolicy`] that handles path canonicalization,
/// overlap deduction, and platform-specific defaults.
///
/// Paths are canonicalized on insert. Non-existent paths are silently skipped;
/// other canonicalization failures (e.g., permission denied) return an error.
///
/// **Deduplication and collapse behavior:**
/// - If a narrower path is already covered by a broader entry in the same or a
///   higher-privilege set, the narrower entry is not added (forward dedup).
/// - If a broader path is added after narrower entries in the same or lower-privilege
///   set, the narrower entries are removed (reverse overlap deduction / intra-set collapse).
/// - Privilege ordering is read < exec < write. A write path subsumes read and exec
///   children; an exec path subsumes read children.
///
/// # Examples
///
/// ```no_run
/// use lot::SandboxPolicyBuilder;
///
/// let policy = SandboxPolicyBuilder::new()
///     .include_platform_exec_paths().expect("exec paths")
///     .include_platform_lib_paths().expect("lib paths")
///     .include_temp_dirs().expect("temp dirs")
///     .allow_network(false)
///     .max_memory_bytes(128 * 1024 * 1024)
///     .max_processes(8)
///     .build()
///     .expect("policy invalid");
/// ```
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicyBuilder {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    exec_paths: Vec<PathBuf>,
    deny_paths: Vec<PathBuf>,
    allow_network: bool,
    limits: ResourceLimits,
    sentinel_dir: Option<PathBuf>,
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

/// Canonicalize a path, returning None for NotFound (silently skipped)
/// and propagating all other I/O errors.
fn try_canonicalize(path: &Path) -> crate::Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(canon) => Ok(Some(canon)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SandboxError::Setup(format!(
            "canonicalize {}: {e}",
            path.display()
        ))),
    }
}

impl SandboxPolicyBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a read-only path. Canonicalized on insert; silently skipped if
    /// non-existent or already covered by a write, read, or exec entry.
    ///
    /// Overlap handling follows the privilege ordering read < exec < write:
    /// - Skips if covered by any same-or-higher privilege set.
    /// - Removes children from same-or-lower privilege sets.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization fails for any
    /// reason other than the path not existing.
    pub fn read_path(mut self, path: impl AsRef<Path>) -> crate::Result<Self> {
        if let Some(canon) = try_canonicalize(path.as_ref())? {
            // Write and exec are supersets of read — skip if already covered.
            if !covered_by(&canon, &self.write_paths)
                && !covered_by(&canon, &self.read_paths)
                && !covered_by(&canon, &self.exec_paths)
            {
                remove_covered_by(&mut self.read_paths, &canon);
                self.read_paths.push(canon);
            }
        }
        Ok(self)
    }

    /// Add a read-write path. Canonicalized on insert; silently skipped if
    /// non-existent or already covered by an existing write entry.
    ///
    /// Overlap handling follows the privilege ordering read < exec < write:
    /// - Skips if covered by the write set (same privilege level).
    /// - Removes children from all sets (write supersedes read and exec).
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization fails for any
    /// reason other than the path not existing.
    pub fn write_path(mut self, path: impl AsRef<Path>) -> crate::Result<Self> {
        if let Some(canon) = try_canonicalize(path.as_ref())? {
            // Skip if already covered by a write parent (same privilege level).
            // Unlike read_path, we do NOT skip for read/exec coverage because
            // write supersedes both.
            if !covered_by(&canon, &self.write_paths) {
                // Write supersedes read and exec entries it covers (children).
                remove_covered_by(&mut self.read_paths, &canon);
                remove_covered_by(&mut self.exec_paths, &canon);
                remove_covered_by(&mut self.write_paths, &canon);
                self.write_paths.push(canon);
            }
        }
        Ok(self)
    }

    /// Add an executable path. Canonicalized on insert; silently skipped if
    /// non-existent or already covered by an existing exec or write entry.
    ///
    /// Overlap handling follows the privilege ordering read < exec < write:
    /// - Skips if covered by exec or write sets (same-or-higher privilege).
    /// - Removes children from read and exec sets (exec supersedes read).
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization fails for any
    /// reason other than the path not existing.
    pub fn exec_path(mut self, path: impl AsRef<Path>) -> crate::Result<Self> {
        if let Some(canon) = try_canonicalize(path.as_ref())? {
            if !covered_by(&canon, &self.exec_paths) && !covered_by(&canon, &self.write_paths) {
                // Exec subsumes read entries it covers.
                remove_covered_by(&mut self.read_paths, &canon);
                remove_covered_by(&mut self.exec_paths, &canon);
                self.exec_paths.push(canon);
            }
        }
        Ok(self)
    }

    /// Add a deny path. Canonicalized on insert; silently skipped if
    /// non-existent. No deduplication against grant paths — the deny is
    /// intentional and preserved.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization fails for any
    /// reason other than the path not existing.
    pub fn deny_path(mut self, path: impl AsRef<Path>) -> crate::Result<Self> {
        if let Some(canon) = try_canonicalize(path.as_ref())? {
            if !self.deny_paths.contains(&canon) {
                self.deny_paths.push(canon);
            }
        }
        Ok(self)
    }

    /// Add multiple deny paths. Convenience wrapper around [`deny_path`](Self::deny_path).
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization of any path fails
    /// for a reason other than the path not existing.
    pub fn deny_paths(
        mut self,
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> crate::Result<Self> {
        for p in paths {
            self = self.deny_path(p)?;
        }
        Ok(self)
    }

    /// Add the platform temp directory to write paths.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization of the temp
    /// directory fails.
    pub fn include_temp_dirs(self) -> crate::Result<Self> {
        self.write_path(std::env::temp_dir())
    }

    /// Add standard platform executable directories to exec paths.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization of any platform
    /// exec path fails for a reason other than the path not existing.
    pub fn include_platform_exec_paths(mut self) -> crate::Result<Self> {
        for p in platform_exec_paths() {
            self = self.exec_path(p)?;
        }
        Ok(self)
    }

    /// Add standard platform library/include directories to read paths.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Setup`] if canonicalization of any platform
    /// lib path fails for a reason other than the path not existing.
    pub fn include_platform_lib_paths(mut self) -> crate::Result<Self> {
        for p in platform_lib_paths() {
            self = self.read_path(p)?;
        }
        Ok(self)
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

    /// Set a custom directory for sentinel files (Windows only).
    ///
    /// By default, sentinel files are written to `std::env::temp_dir()`.
    /// Setting a custom directory scopes sentinel I/O to that path,
    /// which isolates concurrent sandbox sessions from each other.
    #[must_use]
    pub fn sentinel_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.sentinel_dir = Some(dir.into());
        self
    }

    /// Consume the builder and produce a validated [`SandboxPolicy`].
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidPolicy`] if the resulting policy fails
    /// validation (e.g. no paths at all, or zero resource limits).
    pub fn build(self) -> crate::Result<SandboxPolicy> {
        let mut policy = SandboxPolicy::new(
            self.read_paths,
            self.write_paths,
            self.exec_paths,
            self.deny_paths,
            self.allow_network,
            self.limits,
        );
        policy.sentinel_dir = self.sentinel_dir;
        policy.validate()?;
        Ok(policy)
    }
}

/// Platform executable directories (shells, common tools).
///
/// Maintained separately from `platform_implicit_paths` and seatbelt always-allowed paths
/// because each serves a different purpose: builder defaults vs sandbox-mechanism internals.
/// The lists have no meaningful overlap across platforms.
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
        let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
        TempDir::new_in(test_tmp).expect("failed to create temp dir")
    }

    #[test]
    fn basic_build() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .unwrap()
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
            .unwrap()
            .read_path("/surely/does/not/exist/xyz")
            .unwrap()
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
            .unwrap()
            .write_path(tmp.path())
            .unwrap()
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
            .unwrap()
            .read_path(&child)
            .unwrap()
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
            .unwrap()
            .read_path(tmp.path())
            .unwrap()
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
    }

    #[test]
    fn duplicate_write_path_deduplicated() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .write_path(tmp.path())
            .unwrap()
            .write_path(tmp.path())
            .unwrap()
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
            .unwrap()
            .read_path(&child)
            .unwrap()
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
            .unwrap()
            .read_path(tmp.path())
            .unwrap()
            .build()
            .expect("build should succeed");
        assert_eq!(policy.read_paths().len(), 1);
    }

    #[test]
    fn allow_network_flag() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .unwrap()
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
            .unwrap()
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
            .unwrap()
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
            .unwrap()
            .include_temp_dirs()
            .unwrap()
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
            .unwrap()
            .exec_path(&exec_dir)
            .unwrap()
            .exec_path(&exec_dir)
            .unwrap()
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
            .unwrap()
            .write_path(&write_dir)
            .unwrap()
            .allow_network(false)
            .max_memory_bytes(512 * 1024 * 1024)
            .build()
            .expect("build should succeed");

        // The produced policy should pass validate() independently.
        policy.validate().expect("validate should pass");
    }

    #[test]
    fn deny_path_dedup_same_path_twice() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let denied = tmp.path().join("secret");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&parent)
            .unwrap()
            .deny_path(&denied)
            .unwrap()
            .deny_path(&denied)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert_eq!(
            policy.deny_paths().len(),
            1,
            "duplicate deny path should be deduplicated"
        );
    }

    #[test]
    fn deny_path_nonexistent_silently_skipped() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .unwrap()
            .deny_path("/surely/does/not/exist/xyz")
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.deny_paths().is_empty(),
            "nonexistent deny path should be silently skipped"
        );
    }

    #[test]
    fn deny_paths_batch_method() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let d1 = tmp.path().join("a");
        let d2 = tmp.path().join("b");
        std::fs::create_dir(&d1).expect("create dir a");
        std::fs::create_dir(&d2).expect("create dir b");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&parent)
            .unwrap()
            .deny_paths([&d1, &d2])
            .unwrap()
            .build()
            .expect("build should succeed");

        assert_eq!(
            policy.deny_paths().len(),
            2,
            "batch deny_paths should add both"
        );
    }

    // ── cross-set deduction: read ↔ exec ──────────────────────────

    #[test]
    fn read_covered_by_exec_not_added() {
        let tmp = make_temp_dir();
        let dir = tmp.path().join("bin");
        std::fs::create_dir(&dir).expect("create dir");

        let policy = SandboxPolicyBuilder::new()
            .exec_path(&dir)
            .unwrap()
            .read_path(&dir)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.read_paths().is_empty(),
            "read should be skipped when exec covers it"
        );
        assert_eq!(policy.exec_paths().len(), 1);
    }

    #[test]
    fn read_child_under_exec_parent_not_added() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("bin");
        let child = parent.join("sub");
        std::fs::create_dir_all(&child).expect("create dirs");

        let policy = SandboxPolicyBuilder::new()
            .exec_path(&parent)
            .unwrap()
            .read_path(&child)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(policy.read_paths().is_empty());
        assert_eq!(policy.exec_paths().len(), 1);
    }

    #[test]
    fn exec_path_removes_covered_read() {
        let tmp = make_temp_dir();
        let dir = tmp.path().join("lib");
        std::fs::create_dir(&dir).expect("create dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&dir)
            .unwrap()
            .exec_path(&dir)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.read_paths().is_empty(),
            "exec should remove matching read"
        );
        assert_eq!(policy.exec_paths().len(), 1);
    }

    #[test]
    fn exec_parent_removes_read_child() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("lib");
        let child = parent.join("sub");
        std::fs::create_dir_all(&child).expect("create dirs");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&child)
            .unwrap()
            .exec_path(&parent)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.read_paths().is_empty(),
            "exec parent should remove read child"
        );
        assert_eq!(policy.exec_paths().len(), 1);
    }

    // ── cross-set deduction: write ↔ exec ─────────────────────────

    #[test]
    fn exec_covered_by_write_not_added() {
        let tmp = make_temp_dir();
        let dir = tmp.path().join("data");
        std::fs::create_dir(&dir).expect("create dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .unwrap()
            .write_path(&dir)
            .unwrap()
            .exec_path(&dir)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.exec_paths().is_empty(),
            "exec should be skipped when write covers it"
        );
        assert_eq!(policy.write_paths().len(), 1);
    }

    #[test]
    fn write_parent_removes_exec_child() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("data");
        let child = parent.join("bin");
        std::fs::create_dir_all(&child).expect("create dirs");
        // Need a separate read path so the policy has at least one grant
        // outside the write tree for a valid policy.
        let read_dir = tmp.path().join("docs");
        std::fs::create_dir(&read_dir).expect("create docs dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(&read_dir)
            .unwrap()
            .exec_path(&child)
            .unwrap()
            .write_path(&parent)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.exec_paths().is_empty(),
            "write parent should remove exec child"
        );
        assert_eq!(policy.write_paths().len(), 1);
    }

    #[test]
    fn same_path_read_then_exec_passes_validate() {
        let tmp = make_temp_dir();
        let dir = tmp.path().join("shared");
        std::fs::create_dir(&dir).expect("create dir");

        // Cross-set deduction must prevent the same path from appearing
        // in both read and exec sets, which validate() rejects.
        let policy = SandboxPolicyBuilder::new()
            .read_path(&dir)
            .unwrap()
            .exec_path(&dir)
            .unwrap()
            .build()
            .expect("build should succeed — cross-set dedup should prevent overlap");

        policy
            .validate()
            .expect("produced policy must pass validate");
    }

    #[test]
    fn read_parent_after_exec_child_rejected_by_validate() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("lib");
        let child = parent.join("bin");
        std::fs::create_dir_all(&child).expect("create dirs");

        let result = SandboxPolicyBuilder::new()
            .exec_path(&child)
            .unwrap()
            .read_path(&parent)
            .unwrap()
            .build();

        assert!(
            result.is_err(),
            "read parent + exec child is an unresolvable conflict"
        );
    }

    #[test]
    fn exec_parent_with_write_child_rejected_by_validate() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("data");
        let child = parent.join("sub");
        std::fs::create_dir_all(&child).expect("create dirs");

        let result = SandboxPolicyBuilder::new()
            .write_path(&child)
            .unwrap()
            .exec_path(&parent)
            .unwrap()
            .build();

        assert!(
            result.is_err(),
            "exec parent + write child is an unresolvable conflict"
        );
    }

    #[test]
    fn exec_child_under_write_parent_not_added() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("data");
        let child = parent.join("bin");
        std::fs::create_dir_all(&child).expect("create dirs");

        let policy = SandboxPolicyBuilder::new()
            .write_path(&parent)
            .unwrap()
            .exec_path(&child)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert!(
            policy.exec_paths().is_empty(),
            "exec child under write parent should be skipped"
        );
        assert_eq!(policy.write_paths().len(), 1);
    }

    // ── Strengthened error assertions ──────────────────────────────

    #[test]
    fn zero_resource_limit_error_mentions_field() {
        let tmp = make_temp_dir();
        let result = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .unwrap()
            .max_memory_bytes(0)
            .build();
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_memory_bytes"),
            "error should name the field: {msg}"
        );
    }

    #[test]
    fn empty_builder_error_mentions_path() {
        let result = SandboxPolicyBuilder::new().build();
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("at least one path"),
            "error should mention missing paths: {msg}"
        );
    }

    // ── Platform convenience methods ────────────────────────────────

    #[test]
    fn include_platform_exec_paths_succeeds() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .unwrap()
            .include_platform_exec_paths()
            .unwrap()
            .build()
            .expect("build should succeed");
        // On all platforms, at least the read_path should exist.
        assert!(!policy.read_paths().is_empty());
    }

    #[test]
    fn include_platform_lib_paths_succeeds() {
        let tmp = make_temp_dir();
        let policy = SandboxPolicyBuilder::new()
            .exec_path(tmp.path())
            .unwrap()
            .include_platform_lib_paths()
            .unwrap()
            .build()
            .expect("build should succeed");
        assert!(!policy.exec_paths().is_empty());
    }

    // ── Deny path overlap with builder ──────────────────────────────

    #[test]
    fn deny_path_under_write_parent_via_builder() {
        let tmp = make_temp_dir();
        let parent = tmp.path().to_path_buf();
        let denied = tmp.path().join("secret");
        std::fs::create_dir(&denied).expect("create denied dir");

        let policy = SandboxPolicyBuilder::new()
            .write_path(&parent)
            .unwrap()
            .deny_path(&denied)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert_eq!(policy.deny_paths().len(), 1);
        assert_eq!(policy.write_paths().len(), 1);
    }

    // ── Pure logic helpers: covered_by / remove_covered_by ──────────

    #[test]
    fn covered_by_exact_match() {
        let tmp = make_temp_dir();
        let p = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        assert!(covered_by(&p, std::slice::from_ref(&p)));
    }

    #[test]
    fn covered_by_parent() {
        let tmp = make_temp_dir();
        let parent = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let child = parent.join("sub");
        assert!(covered_by(&child, &[parent]));
    }

    #[test]
    fn covered_by_unrelated_returns_false() {
        let a = make_temp_dir();
        let b = make_temp_dir();
        let pa = std::fs::canonicalize(a.path()).expect("canonicalize");
        let pb = std::fs::canonicalize(b.path()).expect("canonicalize");
        assert!(!covered_by(&pa, &[pb]));
    }

    #[test]
    fn remove_covered_by_removes_children() {
        let tmp = make_temp_dir();
        let parent = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let child = parent.join("sub");
        let mut set = vec![child];
        remove_covered_by(&mut set, &parent);
        assert!(set.is_empty());
    }

    #[test]
    fn remove_covered_by_preserves_unrelated() {
        let a = make_temp_dir();
        let b = make_temp_dir();
        let pa = std::fs::canonicalize(a.path()).expect("canonicalize");
        let pb = std::fs::canonicalize(b.path()).expect("canonicalize");
        let mut set = vec![pb.clone()];
        remove_covered_by(&mut set, &pa);
        assert_eq!(set.len(), 1);
        assert_eq!(set[0], pb);
    }

    #[test]
    fn exec_child_then_exec_parent_collapses() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("bin");
        let child = parent.join("sub");
        std::fs::create_dir_all(&child).expect("create dirs");

        let policy = SandboxPolicyBuilder::new()
            .exec_path(&child)
            .unwrap()
            .exec_path(&parent)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert_eq!(policy.exec_paths().len(), 1);
    }

    #[test]
    fn write_child_then_write_parent_collapses() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("data");
        let child = parent.join("sub");
        std::fs::create_dir_all(&child).expect("create dirs");

        let policy = SandboxPolicyBuilder::new()
            .write_path(&child)
            .unwrap()
            .write_path(&parent)
            .unwrap()
            .build()
            .expect("build should succeed");

        assert_eq!(policy.write_paths().len(), 1);
    }
}
