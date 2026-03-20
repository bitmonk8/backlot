pub mod acl_helpers;
mod appcontainer;
mod cmdline;
pub mod elevation;
mod job;
pub mod nul_device;
mod pipe;
pub mod prerequisites;
mod sentinel;
pub mod traverse_acl;

// Shared Win32 constants not exported by `windows-sys` without extra feature flags.
// https://learn.microsoft.com/en-us/windows/win32/fileio/file-access-rights-constants
pub const FILE_GENERIC_READ: u32 = 0x0012_0089;
pub const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
pub const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;

// ── Shared Win32 helpers ─────────────────────────────────────────────

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

/// Encode a `&str` as a null-terminated UTF-16 string for Win32 APIs.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Encode a `Path` as a null-terminated UTF-16 string for Win32 APIs.
pub fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Format a Win32 error code as a human-readable message.
#[allow(clippy::cast_possible_wrap)]
pub fn win32_error_msg(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

pub use appcontainer::WindowsSandboxedChild;

/// Directories Windows makes accessible to AppContainer processes by default.
pub fn platform_implicit_read_paths() -> Vec<std::path::PathBuf> {
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
        appcontainer: appcontainer::available(),
        job_objects: job::available(),
    }
}

pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    appcontainer::spawn(policy, command)
}

pub fn cleanup_stale() -> Result<()> {
    sentinel::cleanup_stale()
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
