use std::path::PathBuf;

/// What a sandboxed process is allowed to do.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Paths the child can read (recursive).
    pub read_paths: Vec<PathBuf>,
    /// Paths the child can read and write (recursive).
    pub write_paths: Vec<PathBuf>,
    /// Paths the child can execute from (recursive).
    pub exec_paths: Vec<PathBuf>,
    /// Allow outbound network access (boolean for v1).
    pub allow_network: bool,
    /// Resource limits.
    pub limits: ResourceLimits,
}

/// Resource constraints for the sandboxed process.
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    /// Maximum memory in bytes. None = no limit.
    pub max_memory_bytes: Option<u64>,
    /// Maximum number of child processes. None = no limit.
    pub max_processes: Option<u32>,
    /// Maximum CPU time in seconds. None = no limit.
    pub max_cpu_seconds: Option<u64>,
}
