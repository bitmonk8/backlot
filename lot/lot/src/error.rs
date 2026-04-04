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
    #[error("AppContainer prerequisites not met: {0}")]
    PrerequisitesNotMet(String),

    /// An underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn display_unsupported() {
        let err = SandboxError::Unsupported("test reason".into());
        assert_eq!(err.to_string(), "platform not supported: test reason");
    }

    #[test]
    fn display_setup() {
        let err = SandboxError::Setup("config failed".into());
        assert_eq!(err.to_string(), "sandbox setup failed: config failed");
    }

    #[test]
    fn display_invalid_policy() {
        let err = SandboxError::InvalidPolicy("bad path".into());
        assert_eq!(err.to_string(), "policy invalid: bad path");
    }

    #[test]
    fn display_cleanup() {
        let err = SandboxError::Cleanup("acl failed".into());
        assert_eq!(err.to_string(), "cleanup failed: acl failed");
    }

    #[test]
    fn display_timeout() {
        let err = SandboxError::Timeout(std::time::Duration::from_secs(10));
        let msg = err.to_string();
        assert!(
            msg.contains("10"),
            "timeout message should contain duration: {msg}"
        );
    }

    #[test]
    fn display_prerequisites_not_met() {
        let err = SandboxError::PrerequisitesNotMet("missing ACE".into());
        assert_eq!(
            err.to_string(),
            "AppContainer prerequisites not met: missing ACE"
        );
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let sandbox_err: SandboxError = io_err.into();
        assert!(matches!(sandbox_err, SandboxError::Io(_)));
        let msg = sandbox_err.to_string();
        assert!(
            msg.contains("file gone"),
            "should preserve io error message: {msg}"
        );
    }

    #[test]
    fn io_variant_display_delegates_to_inner() {
        // #[error(transparent)] means Display delegates to the inner io::Error.
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let sandbox_err: SandboxError = io_err.into();
        let msg = sandbox_err.to_string();
        assert!(
            msg.contains("denied"),
            "Io variant should display inner error: {msg}"
        );
    }

    #[test]
    fn source_returns_none_for_string_variants() {
        use std::error::Error;
        let err = SandboxError::Setup("test".into());
        assert!(err.source().is_none(), "Setup should have no source");
    }

    #[test]
    fn send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<SandboxError>();
        assert_sync::<SandboxError>();
    }
}
