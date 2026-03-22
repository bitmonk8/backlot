//! Cross-platform process sandboxing library.
//!
//! Provides OS-level isolation for spawned child processes. The caller is never
//! sandboxed; only the child inherits the restrictions.
//!
//! # Supported platforms
//!
//! | Platform | Isolation | Resource Limits |
//! |----------|-----------|-----------------|
//! | Linux | User/mount/PID/net/IPC namespaces + seccomp-BPF | cgroups v2 |
//! | macOS | Seatbelt (`sandbox_init` SBPL profiles) | `setrlimit` |
//! | Windows | AppContainer + ACLs | Job Objects |
//!
//! # Quick start
//!
//! ```no_run
//! use lot::{SandboxPolicyBuilder, SandboxCommand, spawn};
//!
//! let policy = SandboxPolicyBuilder::new()
//!     .include_platform_exec_paths().expect("exec paths")
//!     .include_platform_lib_paths().expect("lib paths")
//!     .allow_network(false)
//!     .max_memory_bytes(64 * 1024 * 1024)
//!     .build()
//!     .expect("policy invalid");
//!
//! # #[cfg(unix)]
//! let mut cmd = SandboxCommand::new("/bin/echo");
//! # #[cfg(windows)]
//! # let mut cmd = SandboxCommand::new("cmd.exe");
//! # #[cfg(unix)]
//! cmd.arg("hello from sandbox");
//! # #[cfg(windows)]
//! # cmd.args(["/C", "echo hello from sandbox"]);
//!
//! let child = spawn(&policy, &cmd).expect("spawn failed");
//! let output = child.wait_with_output().expect("wait failed");
//! println!("{}", String::from_utf8_lossy(&output.stdout));
//! ```
//!
//! # Key functions
//!
//! - [`spawn()`] — launch a child process inside a sandbox defined by a [`SandboxPolicy`].
//! - [`probe()`] — detect which sandboxing mechanisms are available on the current platform.
//! - [`cleanup_stale()`] — restore ACLs from crashed sessions (Windows; no-op elsewhere).
//!
//! # Feature flags
//!
//! | Flag | Effect |
//! |------|--------|
//! | `tokio` | Enables [`SandboxedChild::wait_with_output_timeout`] for async wait with timeout. |
//!
//! # CLI and prerequisites
//!
//! See the [project README](https://github.com/nicholasgasior/lot) for CLI usage
//! (`lot run`, `lot setup`, `lot probe`) and Windows AppContainer prerequisites.

mod command;
mod env_check;
mod error;
pub(crate) mod path_util;
mod policy;
mod policy_builder;

#[cfg(unix)]
mod unix;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub use command::{SandboxCommand, SandboxStdio};
pub use error::SandboxError;
pub use policy::{ResourceLimits, SandboxPolicy};
pub use policy_builder::SandboxPolicyBuilder;

/// Result type for sandbox operations.
pub type Result<T> = std::result::Result<T, SandboxError>;

/// Reports which OS-level sandboxing mechanisms are available on the current
/// platform. Obtained via [`probe()`].
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct PlatformCapabilities {
    /// Linux: user namespaces available and permitted.
    pub namespaces: bool,
    /// Linux: seccomp-BPF available.
    pub seccomp: bool,
    /// Linux: cgroups v2 delegation available for the current user.
    pub cgroups_v2: bool,
    /// macOS: Seatbelt (`sandbox_init`) available.
    pub seatbelt: bool,
    /// Windows: `AppContainer` available.
    pub appcontainer: bool,
    /// Windows: Job objects available.
    pub job_objects: bool,
}

/// Check what sandboxing mechanisms are available on the current platform.
///
/// Returns a [`PlatformCapabilities`] struct indicating which OS-level
/// sandboxing mechanisms (namespaces, seccomp, seatbelt, AppContainer, etc.)
/// are available and permitted for the current user.
///
/// # Examples
///
/// ```no_run
/// let caps = lot::probe();
/// if caps.appcontainer {
///     println!("AppContainer available");
/// }
/// if caps.namespaces && caps.seccomp {
///     println!("Linux namespace + seccomp available");
/// }
/// ```
#[allow(clippy::missing_const_for_fn)]
pub fn probe() -> PlatformCapabilities {
    #[cfg(target_os = "linux")]
    return linux::probe();

    #[cfg(target_os = "macos")]
    return macos::probe();

    #[cfg(target_os = "windows")]
    return windows::probe();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: false,
        appcontainer: false,
        job_objects: false,
    };
}

/// Directories each platform implicitly grants to sandboxed processes
/// (libraries, executables, system dirs) regardless of policy.
///
/// Each platform's list is intentionally different — it reflects the dirs that
/// platform's sandbox mechanism auto-mounts or always allows. The lists are
/// maintained in each platform module because they have no meaningful overlap.
pub(crate) fn platform_implicit_paths() -> Vec<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    return linux::platform_implicit_paths();

    #[cfg(target_os = "macos")]
    return macos::platform_implicit_paths();

    #[cfg(target_os = "windows")]
    return windows::platform_implicit_paths();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Vec::new();
}

/// Spawn a sandboxed child process.
///
/// The caller is never sandboxed. The child process inherits
/// the sandbox restrictions and cannot escape them.
///
/// # Errors
///
/// - [`SandboxError::InvalidPolicy`] if policy validation fails.
/// - [`SandboxError::Setup`] if the OS sandbox mechanism cannot be configured
///   (e.g. namespaces disabled, AppContainer creation fails).
/// - [`SandboxError::Unsupported`] on platforms without a sandbox implementation.
/// - [`SandboxError::PrerequisitesNotMet`] on Windows if AppContainer ACL
///   prerequisites are missing.
/// - [`SandboxError::Io`] for underlying OS errors.
///
/// # Examples
///
/// ```no_run
/// use lot::{SandboxPolicyBuilder, SandboxCommand, spawn};
///
/// let policy = SandboxPolicyBuilder::new()
///     .include_platform_exec_paths().expect("exec paths")
///     .include_platform_lib_paths().expect("lib paths")
///     .build()
///     .expect("policy invalid");
///
/// # #[cfg(unix)]
/// let mut cmd = SandboxCommand::new("/bin/echo");
/// # #[cfg(windows)]
/// # let mut cmd = SandboxCommand::new("cmd.exe");
/// # #[cfg(unix)]
/// cmd.arg("hello");
/// # #[cfg(windows)]
/// # cmd.args(["/C", "echo hello"]);
///
/// let child = spawn(&policy, &cmd).expect("spawn failed");
/// let output = child.wait_with_output().expect("wait failed");
/// assert!(output.status.success());
/// ```
pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    // Validate here even though SandboxPolicyBuilder::build() also validates,
    // because callers may construct policies via SandboxPolicy::new() directly.
    policy.validate()?;
    env_check::validate_env_accessibility(policy, command)?;

    #[cfg(target_os = "windows")]
    return windows::spawn(policy, command);

    #[cfg(target_os = "linux")]
    return linux::spawn(policy, command);

    #[cfg(target_os = "macos")]
    return macos::spawn(policy, command);

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = command;
        Err(SandboxError::Unsupported("not yet implemented".into()))
    }
}

/// Restore ACLs from any stale sentinel files left by crashed sessions (Windows).
/// No-op on other platforms.
///
/// When `sentinel_dir` is `Some`, scans that directory for stale sentinels.
/// When `None`, scans the system temp directory (the default location).
///
/// # Errors
///
/// - [`SandboxError::Cleanup`] if ACL restoration fails.
/// - [`SandboxError::Io`] for underlying OS errors.
pub fn cleanup_stale(sentinel_dir: Option<&std::path::Path>) -> Result<()> {
    #[cfg(target_os = "windows")]
    return windows::cleanup_stale(sentinel_dir);

    #[cfg(not(target_os = "windows"))]
    {
        let _ = sentinel_dir;
        Ok(())
    }
}

/// Check whether the current process is running with administrator privileges.
///
/// Windows only. Use this before calling [`grant_appcontainer_prerequisites`] or
/// [`grant_appcontainer_prerequisites_for_policy`] to verify elevation.
#[cfg(target_os = "windows")]
pub use windows::elevation::is_elevated;

/// Check or grant AppContainer prerequisites.
///
/// - [`appcontainer_prerequisites_met`] — check if NUL device and ancestor
///   traverse ACEs are in place for the given paths.
/// - [`grant_appcontainer_prerequisites`] — grant those ACEs (requires elevation).
///
/// The `_for_policy` variants accept a [`SandboxPolicy`] and check/grant
/// prerequisites for all paths (including deny paths) referenced by the policy.
///
/// # Errors
///
/// `grant_appcontainer_prerequisites` and `grant_appcontainer_prerequisites_for_policy`
/// return [`SandboxError::Setup`] if ACE modification fails (e.g. insufficient privileges).
#[cfg(target_os = "windows")]
pub use windows::prerequisites::{
    appcontainer_prerequisites_met, appcontainer_prerequisites_met_for_policy,
    grant_appcontainer_prerequisites, grant_appcontainer_prerequisites_for_policy,
};

/// Grants AppContainer prerequisites for all paths in the policy.
/// No-op on non-Windows platforms.
#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)]
pub fn grant_appcontainer_prerequisites_for_policy(_policy: &SandboxPolicy) -> Result<()> {
    Ok(())
}

/// Checks whether prerequisites are met for all paths in the policy.
/// Always returns `true` on non-Windows platforms.
#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)]
pub fn appcontainer_prerequisites_met_for_policy(_policy: &SandboxPolicy) -> bool {
    true
}

/// A running sandboxed process.
///
/// Created by [`spawn()`]. Dropping the handle performs platform-specific
/// cleanup (ACL restoration on Windows, cgroup removal on Linux, process
/// group termination on macOS).
pub struct SandboxedChild {
    #[cfg(target_os = "windows")]
    inner: windows::WindowsSandboxedChild,
    #[cfg(target_os = "linux")]
    inner: linux::LinuxSandboxedChild,
    #[cfg(target_os = "macos")]
    inner: macos::MacosSandboxedChild,
}

impl std::fmt::Debug for SandboxedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedChild")
            .field("pid", &self.id())
            .finish()
    }
}

/// Macro to dispatch a method call to `self.inner` on supported platforms,
/// returning a fallback on unsupported ones.
macro_rules! platform_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* ), $fallback:expr) => {{
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        return $self.inner.$method($($arg),*);

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        $fallback
    }};
}

impl SandboxedChild {
    /// Returns the OS-assigned process ID.
    pub const fn id(&self) -> u32 {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        return self.inner.id();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        0
    }

    /// Forcibly terminate the sandboxed process.
    pub fn kill(&self) -> std::io::Result<()> {
        platform_dispatch!(
            self,
            kill(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Block until the sandboxed process exits and return its exit status.
    pub fn wait(&self) -> std::io::Result<std::process::ExitStatus> {
        platform_dispatch!(
            self,
            wait(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Non-blocking check: has the process exited?
    pub fn try_wait(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        platform_dispatch!(
            self,
            try_wait(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Wait for the process to exit and collect all stdout/stderr output.
    ///
    /// Consumes the handle. Stdout and stderr must be [`SandboxStdio::Piped`]
    /// (the default) for output to be captured.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use lot::{SandboxPolicyBuilder, SandboxCommand, spawn};
    /// # let policy = SandboxPolicyBuilder::new()
    /// #     .include_platform_exec_paths().unwrap()
    /// #     .include_platform_lib_paths().unwrap()
    /// #     .build().unwrap();
    /// # #[cfg(unix)]
    /// # let cmd = SandboxCommand::new("/bin/echo");
    /// # #[cfg(windows)]
    /// # let cmd = SandboxCommand::new("cmd.exe");
    /// let child = spawn(&policy, &cmd).expect("spawn failed");
    /// let output = child.wait_with_output().expect("wait failed");
    /// println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    /// println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    /// ```
    pub fn wait_with_output(self) -> std::io::Result<std::process::Output> {
        platform_dispatch!(
            self,
            wait_with_output(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Take ownership of the child's stdin pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        platform_dispatch!(self, take_stdin(), None)
    }

    /// Take ownership of the child's stdout pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        platform_dispatch!(self, take_stdout(), None)
    }

    /// Take ownership of the child's stderr pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        platform_dispatch!(self, take_stderr(), None)
    }

    /// Kill the sandboxed process (and all descendants), then run
    /// platform cleanup synchronously. Returns after cleanup is complete.
    ///
    /// Consumes `self` so the `Drop` impl does not run again.
    pub fn kill_and_cleanup(self) -> Result<()> {
        platform_dispatch!(
            self,
            kill_and_cleanup(),
            Err(SandboxError::Unsupported("not yet implemented".into()))
        )
    }

    /// Wait for the child to exit with a timeout. On timeout, kills the
    /// child (and all descendants), runs platform cleanup, and returns
    /// a timeout error.
    ///
    /// Internally uses `spawn_blocking` so the synchronous `wait_with_output`
    /// does not block the tokio runtime. On timeout the child is killed by
    /// raw PID to unblock the waiting thread, then the `JoinHandle` is
    /// awaited to ensure `Drop` cleanup runs before returning.
    #[cfg(feature = "tokio")]
    pub async fn wait_with_output_timeout(
        self,
        timeout: std::time::Duration,
    ) -> Result<std::process::Output> {
        let pid = self.id();

        let handle = tokio::task::spawn_blocking(move || self.wait_with_output());

        // Pin the handle so we can poll it across select! arms without
        // consuming it on the timeout path.
        tokio::pin!(handle);

        tokio::select! {
            join_result = &mut handle => {
                let io_result = join_result.unwrap_or_else(|join_err| {
                    // Propagate panics instead of converting to string.
                    if join_err.is_panic() {
                        std::panic::resume_unwind(join_err.into_panic());
                    }
                    Err(std::io::Error::other("task cancelled"))
                });
                Ok(io_result?)
            }
            () = tokio::time::sleep(timeout) => {
                // Timeout fired — kill the child by raw PID to unblock
                // the blocking thread's wait call.
                kill_by_pid(pid);

                // Await the blocking thread so Drop (platform cleanup)
                // runs to completion before we return.
                match handle.await {
                    Err(join_err) if join_err.is_panic() => {
                        std::panic::resume_unwind(join_err.into_panic());
                    }
                    _ => {}
                }

                Err(SandboxError::Timeout(timeout))
            }
        }
    }
}

/// Send a kill signal to a process by raw PID. Best-effort; errors are
/// ignored because the process may have already exited.
/// Each platform module owns its own guard logic (reject PID 0 / self).
#[cfg(feature = "tokio")]
fn kill_by_pid(pid: u32) {
    #[cfg(target_os = "macos")]
    macos::kill_by_pid(pid);

    #[cfg(target_os = "linux")]
    linux::kill_by_pid(pid);

    #[cfg(target_os = "windows")]
    windows::kill_by_pid(pid);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "windows")]
    fn probe_windows() {
        let caps = probe();
        assert!(caps.appcontainer);
        assert!(caps.job_objects);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn probe_linux() {
        let caps = probe();
        // Cross-platform fields must be false on Linux.
        assert!(!caps.seatbelt, "seatbelt should be false on Linux");
        assert!(!caps.appcontainer, "appcontainer should be false on Linux");
        assert!(!caps.job_objects, "job_objects should be false on Linux");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn probe_macos() {
        let caps = probe();
        assert!(caps.seatbelt);
    }

    #[test]
    fn timeout_error_display() {
        let err = SandboxError::Timeout(std::time::Duration::from_secs(5));
        let msg = err.to_string();
        assert!(
            msg.contains("5s"),
            "expected duration in message, got: {msg}"
        );
    }

    // ── Builder-based Windows test ────────────────────

    #[test]
    #[cfg(target_os = "windows")]
    fn builder_produces_valid_windows_policy() {
        let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp");
        let tmp = tempfile::TempDir::new_in(&test_tmp).expect("create temp dir");

        let policy = SandboxPolicyBuilder::new()
            .read_path(tmp.path())
            .expect("read_path")
            .allow_network(false)
            .max_memory_bytes(256 * 1024 * 1024)
            .build()
            .expect("build via builder should succeed on Windows");

        // Verify the produced policy passes validate() independently.
        policy
            .validate()
            .expect("builder-produced policy must validate");
        assert!(!policy.read_paths().is_empty());
        assert!(!policy.allow_network());
        assert_eq!(policy.limits().max_memory_bytes, Some(256 * 1024 * 1024));
    }

    /// PID 0 must be silently rejected. Reaching the end of this test
    /// confirms the guard prevented the OS call (which would be undefined behavior).
    #[test]
    #[cfg(feature = "tokio")]
    fn kill_by_pid_zero_does_not_panic() {
        kill_by_pid(0);
    }

    /// Killing our own PID must be silently rejected. Reaching the end of
    /// this function confirms the guard worked (a successful self-kill
    /// would prevent execution from reaching this point).
    #[test]
    #[cfg(feature = "tokio")]
    fn kill_by_pid_self_does_not_kill() {
        kill_by_pid(std::process::id());
        // No assertion needed: reaching this line IS the proof.
    }

    /// A nonexistent PID should be silently ignored (best-effort).
    /// No stronger assertion possible for a void FFI function.
    #[test]
    #[cfg(feature = "tokio")]
    fn kill_by_pid_nonexistent_does_not_panic() {
        // u32::MAX is extremely unlikely to be a valid PID.
        kill_by_pid(u32::MAX);
    }
}
