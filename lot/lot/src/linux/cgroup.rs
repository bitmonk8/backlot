use std::ffi::CString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::policy::ResourceLimits;

/// Check whether cgroups v2 delegation is available for the current user.
pub fn is_available() -> bool {
    is_cgroupv2() && has_writable_delegation()
}

/// cgroupv2 is mounted when the unified hierarchy controller list exists.
fn is_cgroupv2() -> bool {
    Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
}

/// Use `access(2)` to check write permission without opening the file.
fn is_writable(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: access() is a read-only check against the filesystem; the
    // CString pointer is valid for the duration of the call.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

/// Check whether the user can create cgroups with resource controllers.
///
/// The process must be in a cgroup whose parent has `subtree_control` set
/// with at least one controller. cgroupv2's "no internal processes" rule
/// means the process's own cgroup cannot have subtree_control with
/// controllers AND contain processes — so we check the parent.
fn has_writable_delegation() -> bool {
    let Some(cgroup_path) = current_cgroup_path() else {
        return false;
    };

    // Check if the parent cgroup has subtree_control with controllers.
    // CgroupGuard creates sibling cgroups under this parent.
    if let Some(parent) = cgroup_path.parent() {
        if parent.starts_with("/sys/fs/cgroup") {
            let subtree_control = parent.join("cgroup.subtree_control");
            if subtree_control.exists() && is_writable(&subtree_control) {
                // Check that at least one controller is enabled.
                if let Ok(contents) = fs::read_to_string(&subtree_control) {
                    if !contents.trim().is_empty() {
                        return true;
                    }
                }
            }
        }
    }

    // Fallback: check current cgroup's subtree_control (works if the process
    // was placed here by systemd before we check).
    let subtree_control = cgroup_path.join("cgroup.subtree_control");
    if subtree_control.exists() && is_writable(&subtree_control) {
        if let Ok(contents) = fs::read_to_string(&subtree_control) {
            return !contents.trim().is_empty();
        }
    }

    false
}

/// Parse /proc/self/cgroup to find the cgroupv2 path.
/// In a unified hierarchy the line is "0::/some/path".
pub fn current_cgroup_path() -> Option<PathBuf> {
    let contents = fs::read_to_string("/proc/self/cgroup").ok()?;
    for line in contents.lines() {
        // cgroupv2 entries start with "0::"
        if let Some(rest) = line.strip_prefix("0::") {
            let relative = rest.trim_start_matches('/');
            return Some(Path::new("/sys/fs/cgroup").join(relative));
        }
    }
    None
}

/// RAII guard for a per-sandbox cgroup directory.
///
/// On creation, creates a unique subdirectory under the current process's
/// delegated cgroup subtree and writes resource limit files. On drop, kills
/// remaining processes and removes the directory.
pub struct CgroupGuard {
    path: PathBuf,
}

impl CgroupGuard {
    /// Create a new cgroup for a sandbox invocation and write resource limits.
    ///
    /// Creates a sibling cgroup under the parent of the current cgroup.
    /// The parent must have controllers enabled in `subtree_control`.
    /// This avoids the cgroupv2 "no internal processes" constraint — the
    /// calling process stays in its leaf cgroup while sandbox children go
    /// into the newly created sibling.
    pub fn new(limits: &ResourceLimits) -> io::Result<Self> {
        let current = current_cgroup_path().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot determine current cgroup path",
            )
        })?;

        let parent = current.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "current cgroup has no parent")
        })?;

        // SAFETY: getpid has no preconditions
        let pid = unsafe { libc::getpid() };

        // Use monotonic clock nanoseconds for uniqueness. On collision
        // (EEXIST), retry with an incremented suffix.
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is a valid timespec on the stack; CLOCK_MONOTONIC is always available.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
        let nanos = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;

        let base_name = format!("lot-sandbox-{pid}-{nanos}");
        let mut path = parent.join(&base_name);
        let mut created = false;
        for attempt in 0u32..=16 {
            if attempt > 0 {
                path = parent.join(format!("{base_name}-{attempt}"));
            }
            match fs::create_dir(&path) {
                Ok(()) => {
                    created = true;
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        if !created {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("cgroup dir exhausted 17 name attempts: {}", path.display()),
            ));
        }

        // Write resource limits. On failure, clean up the directory we created.
        let write_limits = || -> io::Result<()> {
            if let Some(bytes) = limits.max_memory_bytes {
                fs::write(path.join("memory.max"), bytes.to_string())?;
                // Disable swap so memory.max is a hard physical limit.
                // Without this, processes can swap pages and exceed the
                // intended limit on hosts with swap enabled.
                // NotFound is expected when the swap controller is not enabled.
                match fs::write(path.join("memory.swap.max"), "0") {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            if let Some(count) = limits.max_processes {
                fs::write(path.join("pids.max"), count.to_string())?;
            }
            Ok(())
        };

        if let Err(e) = write_limits() {
            let _ = fs::remove_dir(&path);
            return Err(e);
        }

        // max_cpu_seconds: intentionally not enforced via cgroups.
        // cgroupv2 cpu.max controls bandwidth (rate limiting), not total CPU
        // time. See `ResourceLimits::max_cpu_seconds` doc for platform details.

        Ok(Self { path })
    }

    /// Returns the cgroup directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move a process into this cgroup by writing its PID to cgroup.procs.
    #[allow(dead_code)] // only used from tests
    pub(crate) fn add_process(&self, pid: i32) -> io::Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())
    }

    /// Kill all processes remaining in the cgroup.
    ///
    /// Prefers `cgroup.kill` (kernel 5.14+) which atomically kills all processes.
    /// Falls back to reading `cgroup.procs` and sending SIGKILL to each PID,
    /// with a `/proc/{pid}/cgroup` membership check to mitigate PID recycling.
    fn kill_all(&self) {
        // Try atomic cgroup.kill first (available since kernel 5.14).
        let kill_path = self.path.join("cgroup.kill");
        if fs::write(&kill_path, "1").is_ok() {
            return;
        }

        // Fallback: read PIDs and kill individually.
        // A TOCTOU window remains between the membership check and kill(),
        // but it is substantially narrower than killing without any check.
        let procs_path = self.path.join("cgroup.procs");
        let Ok(contents) = fs::read_to_string(&procs_path) else {
            eprintln!(
                "lot: failed to read cgroup.procs for kill fallback: {}",
                procs_path.display()
            );
            return;
        };

        // Derive the expected cgroup relative path from our absolute path.
        // Our path is /sys/fs/cgroup/<relative>, and /proc/PID/cgroup
        // contains "0::/<relative>".
        let cgroup_path_suffix = self
            .path
            .strip_prefix("/sys/fs/cgroup")
            .unwrap_or(&self.path);

        for line in contents.lines() {
            let Ok(pid) = line.trim().parse::<i32>() else {
                continue;
            };
            // Verify the PID still belongs to our cgroup before killing.
            if !Self::pid_in_cgroup(pid, cgroup_path_suffix) {
                continue;
            }
            // SAFETY: kill() with a valid signal number has no UB preconditions.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    /// Check whether a PID's cgroupv2 entry matches the expected path.
    /// Returns `false` if the check cannot be performed (process already exited).
    fn pid_in_cgroup(pid: i32, cgroup_path_suffix: &Path) -> bool {
        let cgroup_file = format!("/proc/{pid}/cgroup");
        let Ok(contents) = fs::read_to_string(cgroup_file) else {
            return false;
        };
        for line in contents.lines() {
            if let Some(rest) = line.strip_prefix("0::") {
                let relative = rest.trim_start_matches('/');
                return Path::new(relative) == cgroup_path_suffix;
            }
        }
        false
    }
}

impl Drop for CgroupGuard {
    fn drop(&mut self) {
        // kill_all() tries cgroup.kill first, then falls back to per-PID SIGKILL.
        self.kill_all();

        // Wait for processes to exit so the cgroup directory becomes empty.
        // Budget: 50 iterations x 20ms = 1s total.
        let mut drained = false;
        for _ in 0..50 {
            let procs_path = self.path.join("cgroup.procs");
            match fs::read_to_string(&procs_path) {
                Ok(contents) if !contents.trim().is_empty() => {
                    // SAFETY: timespec is valid, null second arg means we don't
                    // care about remaining time.
                    unsafe {
                        let ts = libc::timespec {
                            tv_sec: 0,
                            tv_nsec: 20_000_000, // 20ms
                        };
                        libc::nanosleep(&raw const ts, std::ptr::null_mut());
                    }
                }
                _ => {
                    drained = true;
                    break;
                }
            }
        }

        if !drained {
            eprintln!(
                "lot: cgroup still has processes after 1s drain: {}",
                self.path.display()
            );
        }

        // Best-effort removal; ignore failure to avoid panicking in drop.
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn require_cgroups() {
        assert!(is_available(), "cgroups v2 not available");
    }

    /// RAII guard ensures forked child is killed+reaped on all paths
    /// (including assertion failures that unwind).
    struct ChildGuard(i32);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
                libc::waitpid(self.0, std::ptr::null_mut(), 0);
            }
        }
    }

    #[test]
    fn cgroup_available_no_panic() {
        let _result = is_available();
    }

    #[test]
    fn cgroup_guard_creates_and_cleans_up() {
        require_cgroups();
        let limits = ResourceLimits {
            max_memory_bytes: Some(64 * 1024 * 1024),
            max_processes: Some(10),
            max_cpu_seconds: None,
        };
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");
        let path = guard.path().to_path_buf();
        assert!(path.exists(), "cgroup directory should exist");

        // Verify memory.max was written (kernel may append newline)
        let mem_max = fs::read_to_string(path.join("memory.max")).expect("read memory.max");
        assert_eq!(mem_max.trim(), "67108864");

        // Verify pids.max was written
        let pids_max = fs::read_to_string(path.join("pids.max")).expect("read pids.max");
        assert_eq!(pids_max.trim(), "10");

        drop(guard);
        assert!(
            !path.exists(),
            "cgroup directory should be removed after drop"
        );
    }

    #[test]
    fn cgroup_guard_add_process() {
        require_cgroups();
        let limits = ResourceLimits::default();
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");

        // SAFETY: fork() is safe in a single-threaded context; the child
        // immediately sleeps and is killed by the guard.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe { libc::pause() };
            // SAFETY: _exit is async-signal-safe; avoids running Rust destructors in forked child.
            unsafe { libc::_exit(0) };
        }
        let _child_guard = ChildGuard(pid);

        // Parent: add the child to the cgroup.
        guard
            .add_process(pid)
            .expect("add_process must succeed when cgroups are available");
        // Guard drop will kill the child and remove the cgroup.
        // ChildGuard drop ensures the zombie is reaped.
    }

    #[test]
    fn cgroup_guard_no_limits_creates_empty() {
        require_cgroups();
        let limits = ResourceLimits::default();
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");
        let path = guard.path().to_path_buf();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn cgroup_guard_drain_with_live_process() {
        require_cgroups();
        let limits = ResourceLimits::default();
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");
        let path = guard.path().to_path_buf();

        // SAFETY: fork() is safe here; child immediately pauses.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe { libc::pause() };
            // SAFETY: _exit is async-signal-safe; avoids running Rust destructors in forked child.
            unsafe { libc::_exit(0) };
        }
        let _child_guard = ChildGuard(pid);

        guard
            .add_process(pid)
            .expect("add_process must succeed when cgroups are available");

        drop(guard);

        // After drop, the cgroup directory should be removed.
        // The guard's Drop kills all cgroup processes via cgroup.kill,
        // then drains and removes the directory.
        assert!(
            !path.exists(),
            "cgroup directory should be removed after draining live process"
        );
    }
}
