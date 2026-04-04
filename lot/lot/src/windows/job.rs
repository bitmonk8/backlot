#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
};

// UI restriction flags for JOB_OBJECT_BASIC_UI_RESTRICTIONS.
// Values from winnt.h; defined here because windows-sys may not export them
// without extra feature flags.
const JOB_OBJECT_UILIMIT_HANDLES: u32 = 0x0001;
const JOB_OBJECT_UILIMIT_READCLIPBOARD: u32 = 0x0002;
const JOB_OBJECT_UILIMIT_WRITECLIPBOARD: u32 = 0x0004;
const JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0008;
const JOB_OBJECT_UILIMIT_DISPLAYSETTINGS: u32 = 0x0010;
const JOB_OBJECT_UILIMIT_GLOBALATOMS: u32 = 0x0020;
const JOB_OBJECT_UILIMIT_DESKTOP: u32 = 0x0040;
const JOB_OBJECT_UILIMIT_EXITWINDOWS: u32 = 0x0080;

/// All UI restrictions OR'd together (0x00FF).
const ALL_UI_RESTRICTIONS: u32 = JOB_OBJECT_UILIMIT_HANDLES
    | JOB_OBJECT_UILIMIT_READCLIPBOARD
    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
    | JOB_OBJECT_UILIMIT_GLOBALATOMS
    | JOB_OBJECT_UILIMIT_DESKTOP
    | JOB_OBJECT_UILIMIT_EXITWINDOWS;

/// Check whether Job objects are available.
pub const fn is_available() -> bool {
    true
}

/// Wrapper around a Windows Job Object handle.
pub struct JobObject {
    handle: HANDLE,
}

impl JobObject {
    /// Create a new anonymous job object.
    pub fn new() -> io::Result<Self> {
        // SAFETY: CreateJobObjectW with null args creates an anonymous job object.
        // Returns null on failure, never INVALID_HANDLE_VALUE.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Configure the job object with `KILL_ON_JOB_CLOSE` and UI restrictions.
    ///
    /// Closing the handle terminates all processes in the job.
    pub fn configure(&self) -> io::Result<()> {
        // SAFETY: Zeroing a plain-old-data Win32 struct is valid initialization.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };

        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        let info_ptr = &raw const info;
        // The struct size is fixed and always fits in u32.
        #[allow(clippy::cast_possible_truncation)]
        let info_len = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;

        // SAFETY: info is fully initialized (zeroed + fields set above).
        // The pointer and length match the expected type for JobObjectExtendedLimitInformation.
        let ret = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                info_ptr.cast(),
                info_len,
            )
        };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }

        self.set_ui_restrictions()
    }

    /// Apply all UI restrictions: handles, read-clipboard, write-clipboard,
    /// system parameters, display settings, global atoms, desktop, exit-windows.
    fn set_ui_restrictions(&self) -> io::Result<()> {
        let ui_restrict = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: ALL_UI_RESTRICTIONS,
        };

        let ui_ptr = &raw const ui_restrict;
        #[allow(clippy::cast_possible_truncation)]
        let ui_len = std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32;

        // SAFETY: ui_restrict is fully initialized. The pointer and length
        // match the expected type for JobObjectBasicUIRestrictions.
        let ret = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectBasicUIRestrictions,
                ui_ptr.cast(),
                ui_len,
            )
        };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Assign an already-running process to this job object.
    pub fn assign_process(&self, process_handle: HANDLE) -> io::Result<()> {
        // SAFETY: Both handles must be valid. The caller is responsible for
        // providing a valid process handle. self.handle is valid while Self lives.
        let ret = unsafe { AssignProcessToJobObject(self.handle, process_handle) };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Return the raw handle (for use by other Windows APIs).
    #[allow(dead_code)]
    pub const fn as_raw_handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: self.handle was successfully created in new() and has not been closed.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;

    #[test]
    fn create_and_configure() {
        let job = JobObject::new().expect("create job object");
        job.configure().expect("configure job");
    }

    #[test]
    fn assign_child_process() {
        use std::process::Stdio;

        let job = JobObject::new().expect("create job object");
        job.configure().expect("configure job");

        // Use stdin pipe so the child blocks and cannot exit before assignment.
        let mut child = Command::new("cmd")
            .args(["/C", "set /p x="])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let proc_handle = child.as_raw_handle() as HANDLE;
        job.assign_process(proc_handle).expect("assign process");

        // Drop stdin to unblock, then reap the child.
        drop(child.stdin.take());
        let _ = child.wait();
    }

    #[test]
    fn kill_on_close() {
        use std::process::Stdio;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        let job = JobObject::new().expect("create job object");
        job.configure().expect("configure job");

        // Spawn a child that blocks on stdin — guaranteed to stay alive until killed.
        let child = Command::new("cmd")
            .args(["/C", "set /p x="])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let proc_handle = child.as_raw_handle() as HANDLE;
        job.assign_process(proc_handle).expect("assign process");

        // Drop the job — KILL_ON_JOB_CLOSE should terminate the child.
        drop(job);

        // SAFETY: proc_handle is still valid (we hold the Child).
        // Verify the process terminates within 5 seconds.
        let wait_result = unsafe { WaitForSingleObject(proc_handle, 5000) };
        assert_eq!(
            wait_result, WAIT_OBJECT_0,
            "process should have been terminated by job close"
        );
    }

    #[test]
    fn configure_sets_kill_on_close_flag() {
        use windows_sys::Win32::System::JobObjects::QueryInformationJobObject;

        let job = JobObject::new().expect("create job object");
        job.configure().expect("configure job");

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        #[allow(clippy::cast_possible_truncation)]
        let info_len = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        let ret = unsafe {
            QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw mut info).cast(),
                info_len,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            ret,
            0,
            "QueryInformationJobObject failed: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            info.BasicLimitInformation.LimitFlags,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
    }
}
