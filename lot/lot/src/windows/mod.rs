pub mod acl_helpers;
mod appcontainer;
mod cmdline;
pub mod elevation;
mod job;
pub mod nul_device;
mod pipe;
pub mod prerequisites;
pub mod sddl;
mod sentinel;
pub mod traverse_acl;

// Shared Win32 constants not exported by `windows-sys` without extra feature flags.
// https://learn.microsoft.com/en-us/windows/win32/fileio/file-access-rights-constants
pub const FILE_GENERIC_READ: u32 = 0x0012_0089;
pub const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
pub const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;

// ── Shared Win32 helpers ─────────────────────────────────────────────

use std::os::windows::ffi::OsStrExt;

/// Encode an `OsStr`-compatible value as a null-terminated UTF-16 string for Win32 APIs.
/// Works with `&str`, `&OsStr`, and `&Path` via `AsRef<std::ffi::OsStr>`.
pub fn to_wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Format a Win32 error code as a human-readable message.
#[allow(clippy::cast_possible_wrap)]
pub fn win32_error_msg(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

pub use appcontainer::WindowsSandboxedChild;

/// Directories Windows makes accessible to sandboxed processes (auto-mounted or always-allowed).
pub fn platform_implicit_paths() -> Vec<std::path::PathBuf> {
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
    vec![std::path::PathBuf::from(&sys_root)]
}

use crate::command::SandboxCommand;
use crate::policy::SandboxPolicy;
use crate::{PlatformCapabilities, Result, SandboxedChild};

#[allow(clippy::missing_const_for_fn)] // Matches non-const signature on other platforms.
pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: false,
        appcontainer: appcontainer::is_available(),
        job_objects: job::is_available(),
    }
}

pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    appcontainer::spawn(policy, command)
}

pub fn cleanup_stale(dir: Option<&std::path::Path>) -> Result<()> {
    let (stale, scan_errors) = sentinel::find_stale_sentinels(dir)?;
    let mut errors = Vec::new();
    for e in &scan_errors {
        errors.push(format!("scan: {e}"));
    }
    for s in &stale {
        if let Err(e) = sentinel::restore_acls_and_delete_sentinel(s) {
            // Sentinel is preserved on restore failure — skip profile
            // deletion so the next cleanup_stale() call can retry.
            errors.push(format!("{}: {e}", s.profile_name));
            continue;
        }
        if let Err(e) = appcontainer::delete_profile(&s.profile_name) {
            errors.push(format!("delete profile {}: {e}", s.profile_name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(crate::SandboxError::Cleanup(errors.join("; ")))
    }
}

/// Send a terminate signal to a process by raw PID. Best-effort; the
/// process may have already exited. The Job Object (KILL_ON_JOB_CLOSE)
/// handles descendant cleanup when the SandboxedChild is dropped.
#[cfg(feature = "tokio")]
#[allow(unsafe_code)]
pub fn kill_by_pid(pid: u32) {
    if pid == 0 || pid == std::process::id() {
        return;
    }
    // SAFETY: Opening a process handle by PID and terminating it.
    // The handle is closed immediately after.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_basic_string() {
        let result = to_wide("hello");
        let expected: Vec<u16> = "hello".encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn to_wide_empty_string() {
        let result = to_wide("");
        assert_eq!(result, vec![0u16]);
    }

    #[test]
    fn to_wide_unicode() {
        // U+00E9 = 'e' with acute accent, fits in single UTF-16 code unit
        let result = to_wide("\u{00E9}");
        assert_eq!(result, vec![0x00E9, 0]);
    }

    #[test]
    fn to_wide_surrogate_pair() {
        // U+1F389 requires a surrogate pair in UTF-16
        let result = to_wide("\u{1F389}");
        assert_eq!(result, vec![0xD83C, 0xDF89, 0]);
    }

    #[test]
    fn platform_implicit_paths_returns_system_root() {
        let paths = platform_implicit_paths();
        assert!(!paths.is_empty(), "should return at least one path");
        // Should contain the Windows system root
        let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().eq_ignore_ascii_case(&sys_root)),
            "should contain SYSTEMROOT: paths={paths:?}"
        );
    }

    #[test]
    fn to_wide_path_round_trips() {
        let path = std::path::Path::new(r"C:\Windows\System32");
        let wide = to_wide(path);
        // Must end with null terminator
        assert_eq!(*wide.last().unwrap(), 0u16);
        // Decode back (without null)
        let decoded = String::from_utf16_lossy(&wide[..wide.len() - 1]);
        assert_eq!(decoded, r"C:\Windows\System32");
    }

    #[test]
    fn win32_error_msg_returns_nonempty() {
        // Error code 2 = ERROR_FILE_NOT_FOUND
        let msg = win32_error_msg(2);
        assert!(!msg.is_empty(), "error message should not be empty");
    }
}
