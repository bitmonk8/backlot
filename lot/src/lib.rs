//! Cross-platform process sandboxing library.
//!
//! Provides OS-level isolation for spawned child processes. The caller is never
//! sandboxed; only the child inherits the restrictions.
//!
//! # Supported platforms
//!
//! - **Linux** — user/mount/PID/network namespaces, seccomp-BPF, cgroups v2
//! - **macOS** — Seatbelt (`sandbox_init`) profiles
//! - **Windows** — `AppContainer` profiles, Job Objects
//!
//! # Key functions
//!
//! - [`spawn()`] — launch a child process inside a sandbox defined by a [`SandboxPolicy`].
//! - [`probe()`] — detect which sandboxing mechanisms are available on the current platform.
//! - [`cleanup_stale()`] — restore ACLs from crashed sessions (Windows; no-op elsewhere).

mod command;
mod error;
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

/// Spawn a sandboxed child process.
///
/// The caller is never sandboxed. The child process inherits
/// the sandbox restrictions and cannot escape them.
pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    policy.validate()?;

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
#[allow(clippy::missing_const_for_fn)] // Not const on Windows.
pub fn cleanup_stale() -> Result<()> {
    #[cfg(target_os = "windows")]
    return windows::cleanup_stale();

    #[cfg(not(target_os = "windows"))]
    Ok(())
}

#[cfg(target_os = "windows")]
pub use windows::nul_device::{
    can_modify_nul_device, grant_nul_device_access, nul_device_accessible,
};

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
    inner: macos::MacSandboxedChild,
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
                    Err(std::io::Error::other(join_err.to_string()))
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
#[cfg(feature = "tokio")]
#[allow(unsafe_code)]
fn kill_by_pid(pid: u32) {
    #[cfg(unix)]
    {
        let Some(pid_i32) = i32::try_from(pid).ok().filter(|&p| p > 0) else {
            return;
        };
        // macOS children call setsid(), so PGID == PID — negate to kill
        // the entire process group. Linux uses PID namespaces instead;
        // killing the helper collapses the namespace.
        #[cfg(target_os = "macos")]
        let target = -pid_i32;
        #[cfg(not(target_os = "macos"))]
        let target = pid_i32;
        // SAFETY: Sending SIGKILL to a valid pid (or negated PGID on macOS).
        unsafe {
            libc::kill(target, libc::SIGKILL);
        }
    }

    #[cfg(windows)]
    {
        // SAFETY: Opening a process handle by PID and terminating it.
        // The handle is closed immediately after. Best-effort; the
        // process may have already exited. On Windows the Job Object
        // (KILL_ON_JOB_CLOSE) handles descendant cleanup when the
        // SandboxedChild is dropped on the blocking thread.
        unsafe {
            let h = windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                0,
                pid,
            );
            if !h.is_null() {
                windows_sys::Win32::System::Threading::TerminateProcess(h, 1);
                windows_sys::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}

#[cfg(test)]
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
    fn probe_linux_no_panic() {
        // Detection must not panic regardless of environment.
        let _caps = probe();
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
}

#[cfg(test)]
#[cfg(feature = "tokio")]
mod tokio_tests {
    use super::*;

    /// Helper: build a minimal policy and command for timeout tests.
    /// Returns None if the platform sandbox cannot be set up (e.g. missing
    /// namespace support on Linux).
    fn spawn_sleep(seconds: u32) -> Option<SandboxedChild> {
        #[cfg(unix)]
        {
            let policy = SandboxPolicy {
                read_paths: vec![std::path::PathBuf::from("/usr")],
                write_paths: vec![],
                exec_paths: vec![],
                allow_network: false,
                limits: crate::policy::ResourceLimits::default(),
            };
            let mut cmd = SandboxCommand::new("/bin/sleep");
            cmd.arg(seconds.to_string());
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);
            spawn(&policy, &cmd).ok()
        }

        #[cfg(windows)]
        {
            // On Windows, use ping -n <seconds+1> 127.0.0.1 as a sleep substitute.
            // timeout.exe requires console input and doesn't work in piped mode.
            let system_root =
                std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
            let system32 = std::path::PathBuf::from(format!("{system_root}\\System32"));
            let policy = SandboxPolicy {
                read_paths: vec![system32.clone()],
                write_paths: vec![],
                exec_paths: vec![system32],
                allow_network: true,
                limits: crate::policy::ResourceLimits::default(),
            };
            let mut cmd = SandboxCommand::new("ping");
            cmd.args(["-n", &(seconds + 1).to_string(), "127.0.0.1"]);
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);
            spawn(&policy, &cmd).ok()
        }
    }

    #[tokio::test]
    async fn timeout_fires_on_long_running_child() {
        let Some(child) = spawn_sleep(60) else {
            return; // sandbox unavailable
        };

        let result = child
            .wait_with_output_timeout(std::time::Duration::from_millis(200))
            .await;

        match result {
            Err(SandboxError::Timeout(d)) => {
                assert!(
                    d.as_millis() >= 200,
                    "timeout duration should match requested"
                );
            }
            other => panic!("expected Timeout error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fast_child_completes_before_timeout() {
        #[cfg(unix)]
        {
            let policy = SandboxPolicy {
                read_paths: vec![std::path::PathBuf::from("/usr")],
                write_paths: vec![],
                exec_paths: vec![],
                allow_network: false,
                limits: crate::policy::ResourceLimits::default(),
            };
            let mut cmd = SandboxCommand::new("/bin/echo");
            cmd.arg("hello");
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);

            let Ok(child) = spawn(&policy, &cmd) else {
                return;
            };

            let result = child
                .wait_with_output_timeout(std::time::Duration::from_secs(10))
                .await;

            match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert_eq!(stdout.trim(), "hello");
                }
                Err(e) => panic!("expected success, got: {e:?}"),
            }
        }

        #[cfg(windows)]
        {
            let system_root =
                std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
            let system32 = std::path::PathBuf::from(format!("{system_root}\\System32"));
            let policy = SandboxPolicy {
                read_paths: vec![system32.clone()],
                write_paths: vec![],
                exec_paths: vec![system32],
                allow_network: false,
                limits: crate::policy::ResourceLimits::default(),
            };
            let mut cmd = SandboxCommand::new("cmd.exe");
            cmd.args(["/C", "echo hello"]);
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);

            let Ok(child) = spawn(&policy, &cmd) else {
                return;
            };

            let result = child
                .wait_with_output_timeout(std::time::Duration::from_secs(10))
                .await;

            match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        stdout.contains("hello"),
                        "expected 'hello' in output, got: {stdout}"
                    );
                }
                Err(e) => panic!("expected success, got: {e:?}"),
            }
        }
    }
}
