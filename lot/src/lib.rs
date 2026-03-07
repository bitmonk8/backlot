// TODO: remove once platform backends consume these types
#![allow(dead_code)]

mod command;
mod error;
mod policy;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub use command::SandboxCommand;
pub use error::SandboxError;
pub use policy::{ResourceLimits, SandboxPolicy};

/// Result type for sandbox operations.
pub type Result<T> = std::result::Result<T, SandboxError>;

/// What sandboxing mechanisms are available on the current platform.
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
pub const fn probe() -> PlatformCapabilities {
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
pub fn spawn(_policy: &SandboxPolicy, _command: SandboxCommand) -> Result<SandboxedChild> {
    Err(SandboxError::Unsupported("not yet implemented".into()))
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
pub struct SandboxedChild {
    _pid: u32,
}
