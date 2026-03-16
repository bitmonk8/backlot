/// Errors returned by sandbox creation, policy validation, and cleanup operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// The current platform does not support the requested sandboxing mechanism.
    #[error("platform not supported: {0}")]
    Unsupported(String),

    /// Sandbox creation failed during OS-level setup.
    #[error("sandbox setup failed: {0}")]
    Setup(String),

    /// The supplied [`SandboxPolicy`](crate::SandboxPolicy) is invalid.
    #[error("policy invalid: {0}")]
    InvalidPolicy(String),

    /// Post-session cleanup (e.g. ACL restoration) failed.
    #[error("cleanup failed: {0}")]
    Cleanup(String),

    /// The child process did not exit within the requested timeout.
    /// The process (and all descendants) has been killed and cleaned up.
    #[error("child process timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// AppContainer prerequisites (ancestor traverse ACEs and/or NUL device
    /// ACE) are not in place for the paths referenced by the policy.
    #[error(
        "AppContainer prerequisites not met: {missing_paths:?}, nul_device_missing={nul_device_missing}"
    )]
    PrerequisitesNotMet {
        /// Ancestor directories missing traverse ACEs.
        missing_paths: Vec<std::path::PathBuf>,
        /// Whether the NUL device ACE is missing.
        nul_device_missing: bool,
    },

    /// An underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
