#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_TIME, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

use crate::policy::ResourceLimits;

/// Check whether Job objects are available.
pub const fn available() -> bool {
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

    /// Apply resource limits from a `ResourceLimits` policy to this job object.
    ///
    /// Always sets `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so that closing the
    /// handle terminates all processes in the job.
    pub fn set_limits(&self, limits: &ResourceLimits) -> io::Result<()> {
        // SAFETY: Zeroing a plain-old-data Win32 struct is valid initialization.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };

        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if let Some(mem) = limits.max_memory_bytes {
            // On 32-bit targets this truncates values >4GB, which is acceptable
            // since the OS cannot enforce a limit larger than the address space.
            #[allow(clippy::cast_possible_truncation)]
            let mem_usize = mem as usize;
            info.ProcessMemoryLimit = mem_usize;
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        }

        if let Some(procs) = limits.max_processes {
            info.BasicLimitInformation.ActiveProcessLimit = procs;
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        }

        if let Some(cpu_secs) = limits.max_cpu_seconds {
            // PerJobUserTimeLimit is in 100-nanosecond intervals.
            // Wrapping is acceptable — values that large exceed any real limit.
            #[allow(clippy::cast_possible_wrap)]
            let ticks = (cpu_secs.saturating_mul(10_000_000)) as i64;
            info.BasicLimitInformation.PerJobUserTimeLimit = ticks;
            flags |= JOB_OBJECT_LIMIT_JOB_TIME;
        }

        info.BasicLimitInformation.LimitFlags = flags;

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
    fn create_and_set_limits() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: Some(100 * 1024 * 1024),
            max_processes: Some(5),
            max_cpu_seconds: Some(30),
        };
        job.set_limits(&limits).expect("set limits");
    }

    #[test]
    fn assign_child_process() {
        use std::process::Stdio;

        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: None,
            max_processes: None,
            max_cpu_seconds: None,
        };
        job.set_limits(&limits).expect("set limits");

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
        let limits = ResourceLimits::default();
        job.set_limits(&limits).expect("set limits");

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

    fn query_limits(job: &JobObject) -> JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
        use windows_sys::Win32::System::JobObjects::QueryInformationJobObject;
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
        info
    }

    #[test]
    fn set_memory_limit_only() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: Some(50 * 1024 * 1024),
            max_processes: None,
            max_cpu_seconds: None,
        };
        job.set_limits(&limits).expect("set memory limit");
        let info = query_limits(&job);
        assert!(info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_PROCESS_MEMORY != 0);
        assert_eq!(info.ProcessMemoryLimit, 50 * 1024 * 1024);
    }

    #[test]
    fn set_process_limit_only() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: None,
            max_processes: Some(3),
            max_cpu_seconds: None,
        };
        job.set_limits(&limits).expect("set process limit");
        let info = query_limits(&job);
        assert!(info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0);
        assert_eq!(info.BasicLimitInformation.ActiveProcessLimit, 3);
    }

    #[test]
    fn set_cpu_limit_only() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: None,
            max_processes: None,
            max_cpu_seconds: Some(60),
        };
        job.set_limits(&limits).expect("set cpu limit");
        let info = query_limits(&job);
        assert!(info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_TIME != 0);
        assert_eq!(
            info.BasicLimitInformation.PerJobUserTimeLimit,
            60 * 10_000_000
        );
    }

    #[test]
    fn set_no_limits() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits::default();
        job.set_limits(&limits).expect("set no limits");
        let info = query_limits(&job);
        assert_eq!(
            info.BasicLimitInformation.LimitFlags,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
    }

    // ── Overflow/saturation ──────────────────────────────────────────

    #[test]
    fn set_limits_u64_max_memory_does_not_panic() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: Some(u64::MAX),
            max_processes: None,
            max_cpu_seconds: None,
        };
        // Should succeed without panic; the value truncates on 32-bit but
        // is valid on 64-bit.
        job.set_limits(&limits)
            .expect("set limits with u64::MAX memory");
    }

    #[test]
    fn set_limits_u64_max_cpu_seconds_does_not_panic() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: None,
            max_processes: None,
            max_cpu_seconds: Some(u64::MAX),
        };
        // saturating_mul prevents overflow; the resulting i64 cast wraps
        // but should not panic.
        job.set_limits(&limits)
            .expect("set limits with u64::MAX cpu");
    }

    #[test]
    fn set_limits_u32_max_processes_does_not_panic() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: None,
            max_processes: Some(u32::MAX),
            max_cpu_seconds: None,
        };
        job.set_limits(&limits)
            .expect("set limits with u32::MAX processes");
        let info = query_limits(&job);
        assert_eq!(info.BasicLimitInformation.ActiveProcessLimit, u32::MAX);
    }

    #[test]
    fn set_limits_all_at_max_does_not_panic() {
        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            max_memory_bytes: Some(u64::MAX),
            max_processes: Some(u32::MAX),
            max_cpu_seconds: Some(u64::MAX),
        };
        job.set_limits(&limits)
            .expect("set all limits to max values");
    }

    // ── memory_limit_kills_child ──────────────────────────────────
    // Note (12.9): PowerShell's CLR may reserve additional memory beyond
    // the script's allocation. The 10 MB limit is low enough that even
    // CLR baseline allocation triggers the job limit. If this test becomes
    // flaky, increase the limit or switch to a native allocator binary.

    #[test]
    fn memory_limit_kills_child() {
        use std::process::Stdio;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        let job = JobObject::new().expect("create job object");
        let limits = ResourceLimits {
            // 10 MB limit — the child will try to exceed this.
            max_memory_bytes: Some(10 * 1024 * 1024),
            max_processes: None,
            max_cpu_seconds: None,
        };
        job.set_limits(&limits).expect("set limits");

        // PowerShell script that allocates memory in a loop until killed.
        let mut child = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "$a = @(); while($true) { $a += [byte[]]::new(1MB) }",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        let proc_handle = child.as_raw_handle() as HANDLE;
        job.assign_process(proc_handle).expect("assign process");

        // SAFETY: proc_handle is valid for the lifetime of child.
        // Wait up to 30 seconds for the memory limit to kill the process.
        let wait_result = unsafe { WaitForSingleObject(proc_handle, 30_000) };

        assert_eq!(
            wait_result, WAIT_OBJECT_0,
            "memory limit must kill child within 30s timeout"
        );

        // Process was terminated by the memory limit.
        let _ = child.wait();
    }
}
