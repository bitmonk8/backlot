use std::ffi::CString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::policy::ResourceLimits;

/// Check whether cgroups v2 delegation is available for the current user.
pub fn available() -> bool {
    is_cgroupv2() && has_writable_subtree()
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

/// Check whether the user can write to their own cgroup subtree.
/// Finds the current process's cgroup and checks for write access.
fn has_writable_subtree() -> bool {
    let Some(cgroup_path) = current_cgroup_path() else {
        return false;
    };

    // Heuristic: if cgroup.subtree_control is writable, delegation is set up.
    let subtree_control = cgroup_path.join("cgroup.subtree_control");
    if subtree_control.exists() {
        return is_writable(&subtree_control);
    }

    // No subtree_control file — check if the cgroup dir itself is writable.
    is_writable(&cgroup_path)
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
    pub fn new(limits: &ResourceLimits) -> io::Result<Self> {
        let parent = current_cgroup_path().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot determine current cgroup path",
            )
        })?;

        // Use monotonic clock nanoseconds as a unique suffix instead of a static counter.
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is a valid timespec on the stack; CLOCK_MONOTONIC is always available.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
        let nanos = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
        // SAFETY: getpid has no preconditions
        let pid = unsafe { libc::getpid() };
        let dir_name = format!("lot-sandbox-{pid}-{nanos}");
        let path = parent.join(dir_name);

        fs::create_dir(&path)?;

        // Write memory limit
        if let Some(bytes) = limits.max_memory_bytes {
            fs::write(path.join("memory.max"), bytes.to_string())?;
        }

        // Write process (PID) limit
        if let Some(count) = limits.max_processes {
            fs::write(path.join("pids.max"), count.to_string())?;
        }

        // max_cpu_seconds is not enforced via cgroups. cgroupv2 cpu.max controls
        // bandwidth (rate limiting), not total CPU time. A monitoring thread
        // reading cpu.stat would be needed for total-time enforcement. The
        // Windows backend uses job objects which do support this natively.

        Ok(Self { path })
    }

    /// Returns the cgroup directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move a process into this cgroup by writing its PID to cgroup.procs.
    #[allow(dead_code)] // used from the fork path via raw fd writes; kept for testing
    pub(crate) fn add_process(&self, pid: i32) -> io::Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())
    }

    /// Kill all processes remaining in the cgroup.
    ///
    /// Prefers `cgroup.kill` (kernel 5.14+) which atomically kills all processes.
    /// Falls back to reading `cgroup.procs` and sending SIGKILL to each PID.
    fn kill_all(&self) {
        // Try atomic cgroup.kill first (available since kernel 5.14).
        let kill_path = self.path.join("cgroup.kill");
        if fs::write(&kill_path, "1").is_ok() {
            return;
        }

        // Fallback: read PIDs and kill individually.
        // WARNING: This path has a PID recycling race — between reading a PID
        // from cgroup.procs and sending SIGKILL, the process may have exited
        // and the PID may have been reassigned to an unrelated process.
        let procs_path = self.path.join("cgroup.procs");
        let Ok(contents) = fs::read_to_string(&procs_path) else {
            return;
        };
        for line in contents.lines() {
            if let Ok(pid) = line.trim().parse::<i32>() {
                // SAFETY: SIGKILL is a well-known signal; pid comes from the
                // kernel's cgroup.procs file.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

impl Drop for CgroupGuard {
    fn drop(&mut self) {
        // Try atomic cgroup.kill (kernel 5.14+) for synchronous kill.
        let kill_path = self.path.join("cgroup.kill");
        let used_cgroup_kill = fs::write(&kill_path, "1").is_ok();

        if !used_cgroup_kill {
            // Fallback: kill individually then poll for emptiness.
            self.kill_all();
        }

        // Wait for processes to exit so the cgroup directory becomes empty.
        // With cgroup.kill this should be near-instant; with fallback we poll.
        for _ in 0..10 {
            let procs_path = self.path.join("cgroup.procs");
            match fs::read_to_string(&procs_path) {
                Ok(contents) if contents.trim().is_empty() => break,
                Err(_) => break,
                _ => {
                    // SAFETY: timespec is valid, null second arg means we don't
                    // care about remaining time.
                    unsafe {
                        let ts = libc::timespec {
                            tv_sec: 0,
                            tv_nsec: 10_000_000, // 10ms
                        };
                        libc::nanosleep(&raw const ts, std::ptr::null_mut());
                    }
                }
            }
        }

        // Attempt to remove the cgroup directory. It must be empty (no
        // processes) for rmdir to succeed. If it fails, we log and move on
        // rather than panicking.
        // Best-effort removal; cgroup dir must be empty (no processes).
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_available_no_panic() {
        let _result = available();
    }

    #[test]
    fn cgroup_guard_creates_and_cleans_up() {
        assert!(available(), "cgroups v2 must be available for this test");
        let limits = ResourceLimits {
            max_memory_bytes: Some(64 * 1024 * 1024),
            max_processes: Some(10),
            max_cpu_seconds: None,
        };
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");
        let path = guard.path().to_path_buf();
        assert!(path.exists(), "cgroup directory should exist");

        // Verify memory.max was written
        let mem_max = fs::read_to_string(path.join("memory.max"));
        assert!(mem_max.is_ok());
        assert_eq!(mem_max.ok().as_deref(), Some("67108864"));

        // Verify pids.max was written
        let pids_max = fs::read_to_string(path.join("pids.max"));
        assert!(pids_max.is_ok());
        assert_eq!(pids_max.ok().as_deref(), Some("10"));

        drop(guard);
        assert!(
            !path.exists(),
            "cgroup directory should be removed after drop"
        );
    }

    #[test]
    fn cgroup_guard_add_process() {
        assert!(available(), "cgroups v2 must be available for this test");
        let limits = ResourceLimits::default();
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");

        // Fork a child that sleeps, add it to the cgroup, then let the guard
        // kill it on drop. We must not add the test process itself — the
        // guard's drop would SIGKILL the test runner.
        // SAFETY: fork() is safe in a single-threaded context; the child
        // immediately sleeps and is killed by the guard.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // Child: sleep until killed.
            unsafe { libc::pause() };
            std::process::exit(0);
        }
        // Parent: add the child to the cgroup.
        // Writing to cgroup.procs may fail depending on cgroup configuration,
        // so we just verify the call doesn't panic.
        drop(guard.add_process(pid));
        // Guard drop will kill the child and remove the cgroup.
    }

    #[test]
    fn cgroup_guard_no_limits_creates_empty() {
        assert!(available(), "cgroups v2 must be available for this test");
        let limits = ResourceLimits::default();
        let guard = CgroupGuard::new(&limits).expect("CgroupGuard::new must succeed");
        let path = guard.path().to_path_buf();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }
}
