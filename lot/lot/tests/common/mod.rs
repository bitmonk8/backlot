//! Shared test helpers for integration tests.
//!
//! Imported via `mod common;` from each integration test file.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::path::PathBuf;
use tempfile::TempDir;

/// Create temp dir inside the project to avoid system temp ancestors
/// (e.g. `C:\Users`) that require elevation for traverse ACE grants.
pub fn make_temp_dir() -> TempDir {
    let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_tmp");
    std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
    TempDir::new_in(&test_tmp).expect("create temp dir")
}

/// Set sandbox-safe overrides for path-bearing env vars, then forward
/// remaining parent env. On Windows, `forward_common_env` skips keys already
/// set, so overrides take priority. No-op on non-Windows (Unix builds an
/// explicit envp without inheriting parent env).
/// `scratch` must be a write_path in the policy (used for TEMP/TMP/TMPDIR).
#[cfg(target_os = "windows")]
pub fn set_sandbox_env(cmd: &mut lot::SandboxCommand, scratch: &std::path::Path) {
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
    let sys32 = format!(r"{sys_root}\System32");
    cmd.env("PATH", &sys32);
    cmd.env("TEMP", scratch);
    cmd.env("TMP", scratch);
    cmd.env("TMPDIR", scratch);
    cmd.forward_common_env();
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)] // empty stub; clippy flags it but const fn with &mut is unstable
pub fn set_sandbox_env(_cmd: &mut lot::SandboxCommand, _scratch: &std::path::Path) {}

/// Platform-appropriate exec_paths for sandbox policies.
///
/// Windows: empty (AppContainer inherits system binary access).
/// Linux: `/bin` and `/usr/bin` if they exist and are distinct.
/// macOS: `/bin` and `/usr/bin`.
#[allow(clippy::missing_const_for_fn)] // cfg strips linux/macos bodies on Windows
pub fn platform_exec_paths() -> Vec<PathBuf> {
    #[allow(unused_mut)]
    let mut paths = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/bin").exists() {
            paths.push(PathBuf::from("/bin"));
        }
        if std::path::Path::new("/usr/bin").exists()
            && std::fs::canonicalize("/usr/bin").ok() != std::fs::canonicalize("/bin").ok()
        {
            paths.push(PathBuf::from("/usr/bin"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/bin"));
        paths.push(PathBuf::from("/usr/bin"));
    }

    paths
}
