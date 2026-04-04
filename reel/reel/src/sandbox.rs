// Re-exports of lot's sandbox prerequisite APIs.
//
// Consumers use these to check/grant the OS-level prerequisites that
// lot's AppContainer sandbox requires on Windows. On non-Windows
// platforms the functions are no-ops that always succeed.

pub use lot::{SandboxError, SandboxPolicy};

/// Check whether the current process is elevated (administrator on Windows).
///
/// Always returns `false` on non-Windows platforms.
#[cfg(target_os = "windows")]
pub use lot::is_elevated;

#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)]
pub fn is_elevated() -> bool {
    false
}

/// One-time elevated setup: grants all ACEs needed for AppContainer
/// sandboxes to function on Windows. Idempotent.
#[cfg(target_os = "windows")]
pub use lot::grant_appcontainer_prerequisites;

#[cfg(not(target_os = "windows"))]
#[allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)]
pub fn grant_appcontainer_prerequisites(_paths: &[&std::path::Path]) -> Result<(), SandboxError> {
    Ok(())
}

/// Grants AppContainer prerequisites for all paths in a `SandboxPolicy`.
pub use lot::grant_appcontainer_prerequisites_for_policy;

/// Checks whether all AppContainer prerequisites are met for the given paths.
#[cfg(target_os = "windows")]
pub use lot::appcontainer_prerequisites_met;

#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)]
pub fn appcontainer_prerequisites_met(_paths: &[&std::path::Path]) -> bool {
    true
}

/// Checks prerequisites for all paths referenced by a `SandboxPolicy`.
pub use lot::appcontainer_prerequisites_met_for_policy;
