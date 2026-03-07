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

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub use command::{SandboxCommand, SandboxStdio};
pub use error::SandboxError;
pub use policy::{ResourceLimits, SandboxPolicy};

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
pub fn cleanup_stale() -> Result<()> {
    #[cfg(target_os = "windows")]
    return windows::cleanup_stale();

    #[cfg(not(target_os = "windows"))]
    Ok(())
}

/// A running sandboxed process.
///
/// Created by [`spawn()`]. Dropping the handle performs platform-specific
/// cleanup (ACL restoration on Windows, cgroup removal on Linux, process
/// termination on macOS).
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

impl SandboxedChild {
    /// Returns the OS-assigned process ID.
    pub const fn id(&self) -> u32 {
        #[cfg(target_os = "windows")]
        return self.inner.id();

        #[cfg(target_os = "linux")]
        return self.inner.id();

        #[cfg(target_os = "macos")]
        return self.inner.id();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        0
    }

    /// Forcibly terminate the sandboxed process.
    pub fn kill(&self) -> std::io::Result<()> {
        #[cfg(target_os = "windows")]
        return self.inner.kill();

        #[cfg(target_os = "linux")]
        return self.inner.kill();

        #[cfg(target_os = "macos")]
        return self.inner.kill();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "not implemented",
        ))
    }

    /// Block until the sandboxed process exits and return its exit status.
    pub fn wait(&self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(target_os = "windows")]
        return self.inner.wait();

        #[cfg(target_os = "linux")]
        return self.inner.wait();

        #[cfg(target_os = "macos")]
        return self.inner.wait();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "not implemented",
        ))
    }

    /// Non-blocking check: has the process exited?
    pub fn try_wait(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        #[cfg(target_os = "windows")]
        return self.inner.try_wait();

        #[cfg(target_os = "linux")]
        return self.inner.try_wait();

        #[cfg(target_os = "macos")]
        return self.inner.try_wait();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "not implemented",
        ))
    }

    /// Wait for the process to exit and collect all stdout/stderr output.
    pub fn wait_with_output(self) -> std::io::Result<std::process::Output> {
        #[cfg(target_os = "windows")]
        return self.inner.wait_with_output();

        #[cfg(target_os = "linux")]
        return self.inner.wait_with_output();

        #[cfg(target_os = "macos")]
        return self.inner.wait_with_output();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "not implemented",
        ))
    }

    /// Take ownership of the child's stdin pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        #[cfg(target_os = "windows")]
        return self.inner.take_stdin();

        #[cfg(target_os = "linux")]
        return self.inner.take_stdin();

        #[cfg(target_os = "macos")]
        return self.inner.take_stdin();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        None
    }

    /// Take ownership of the child's stdout pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        #[cfg(target_os = "windows")]
        return self.inner.take_stdout();

        #[cfg(target_os = "linux")]
        return self.inner.take_stdout();

        #[cfg(target_os = "macos")]
        return self.inner.take_stdout();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        None
    }

    /// Take ownership of the child's stderr pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        #[cfg(target_os = "windows")]
        return self.inner.take_stderr();

        #[cfg(target_os = "linux")]
        return self.inner.take_stderr();

        #[cfg(target_os = "macos")]
        return self.inner.take_stderr();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        None
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
}
