/// Errors that can occur during sandbox operations.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("platform not supported: {0}")]
    Unsupported(String),

    #[error("sandbox setup failed: {0}")]
    Setup(String),

    #[error("policy invalid: {0}")]
    InvalidPolicy(String),

    #[error("cleanup failed: {0}")]
    Cleanup(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
