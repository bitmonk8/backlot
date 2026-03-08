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

    /// An underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
