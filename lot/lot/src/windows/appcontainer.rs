#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    DENY_ACCESS, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SET_ACCESS, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    AllocateAndInitializeSid, DACL_SECURITY_INFORMATION, FreeSid,
    PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Performance::QueryPerformanceCounter;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcessId, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::core::HRESULT;

use crate::Result;
use crate::command::{SandboxCommand, SandboxStdio};
use crate::error::SandboxError;
use crate::policy::SandboxPolicy;

use super::acl_helpers::{ELEVATION_REQUIRED_MARKER, modify_dacl};
use super::cmdline::{build_command_line, build_env_block};
use super::job::JobObject;
use super::pipe::{
    close_handle_if_valid, close_optional_handle, resolve_stdio_input, resolve_stdio_output,
};
use super::sentinel::{SentinelFile, restore_from_sentinel, write_sentinel};

// ── Constants ────────────────────────────────────────────────────────

use super::acl_helpers::SECURITY_APP_PACKAGE_AUTHORITY;
const SECURITY_CAPABILITY_INTERNET_CLIENT: u32 = 1;
const SECURITY_BUILTIN_APP_PACKAGE_RID_COUNT: u8 = 2;
const SECURITY_CAPABILITY_BASE_RID: u32 = 3;

use super::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE};

/// Check whether `AppContainer` is available on this Windows version.
pub const fn available() -> bool {
    true
}

// ── Helpers ──────────────────────────────────────────────────────────

use super::{path_to_wide, to_wide};

fn unique_profile_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Atomic counter ensures uniqueness across concurrent calls from different threads.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    // SAFETY: Side-effect-free queries returning process ID and tick count.
    let pid = unsafe { GetCurrentProcessId() };
    let tick = unsafe { GetTickCount64() };
    let mut qpc: i64 = 0;
    // SAFETY: QueryPerformanceCounter always succeeds on Windows XP+.
    unsafe {
        QueryPerformanceCounter(&raw mut qpc);
    }
    format!("lot-{pid}-{tick}-{qpc}-{seq}")
}

fn hresult_to_io(hr: HRESULT) -> io::Error {
    io::Error::from_raw_os_error(hr)
}

// ── AppContainer profile lifecycle ───────────────────────────────────

/// Create an `AppContainer` profile. Returns `(profile_name, SID)`.
/// The SID must be freed with `FreeSid` when done.
fn create_profile() -> io::Result<(String, PSID)> {
    let name = unique_profile_name();
    let wide_name = to_wide(&name);
    let display = to_wide(&name);
    let desc = to_wide("lot sandbox");
    let mut sid: PSID = std::ptr::null_mut();

    // SAFETY: Valid wide strings and pointer for output SID.
    let hr = unsafe {
        CreateAppContainerProfile(
            wide_name.as_ptr(),
            display.as_ptr(),
            desc.as_ptr(),
            std::ptr::null(),
            0,
            &raw mut sid,
        )
    };

    #[allow(clippy::cast_possible_wrap)]
    let already_exists = ERROR_ALREADY_EXISTS as i32;

    if hr == already_exists {
        delete_profile(&name)?;
        // SAFETY: Retrying after deleting stale profile.
        let hr2 = unsafe {
            CreateAppContainerProfile(
                wide_name.as_ptr(),
                display.as_ptr(),
                desc.as_ptr(),
                std::ptr::null(),
                0,
                &raw mut sid,
            )
        };
        if hr2 != 0 {
            return Err(hresult_to_io(hr2));
        }
    } else if hr != 0 {
        return Err(hresult_to_io(hr));
    }
    Ok((name, sid))
}

pub fn delete_profile(name: &str) -> io::Result<()> {
    let wide = to_wide(name);
    // SAFETY: Valid null-terminated wide string.
    let hr = unsafe { DeleteAppContainerProfile(wide.as_ptr()) };
    if hr != 0 {
        return Err(hresult_to_io(hr));
    }
    Ok(())
}

// ── ACL management ───────────────────────────────────────────────────

/// Add or modify an ACE for `sid` on `path` with the given access mode and mask.
/// Delegates to the shared `modify_dacl` primitive.
fn apply_ace(sid: PSID, path: &Path, access_mode: i32, access_mask: u32) -> io::Result<()> {
    let wide_path = path_to_wide(path);
    let display = path.display().to_string();

    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_UNKNOWN,
        // ptstrName is reinterpreted as a SID pointer when TrusteeForm is TRUSTEE_IS_SID.
        ptstrName: sid.cast(),
    };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access_mask,
        grfAccessMode: access_mode,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: trustee,
    };

    modify_dacl(&wide_path, &display, &[ea], DACL_SECURITY_INFORMATION)
        .map_err(|e| io::Error::other(e.to_string()))
}

fn grant_access(sid: PSID, path: &Path, writable: bool) -> io::Result<()> {
    let access_mask = if writable {
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE
    } else {
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
    };
    apply_ace(sid, path, SET_ACCESS, access_mask)
}

/// Add an explicit deny ACE for the AppContainer SID on a path.
///
/// The parent directory may have an inheritable allow ACE (from `grant_access`).
/// Simply adding a deny ACE is insufficient because Windows evaluates inherited
/// allows before inherited denies when both arrive from different ancestors.
///
/// To guarantee the deny takes effect, this function first protects the denied
/// directory's DACL (`PROTECTED_DACL_SECURITY_INFORMATION`), which converts
/// inherited ACEs to explicit and blocks further inheritance from the parent.
/// Then it adds the explicit deny ACE, which `SetEntriesInAclW` places before
/// explicit allows in canonical DACL order.
fn deny_all_file_access(sid: PSID, path: &Path) -> io::Result<()> {
    let access_mask = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE;

    if path.is_dir() {
        // Step 1: Protect the DACL to stop the parent's allow ACE from
        // propagating further. Converts inherited ACEs to explicit.
        protect_dacl(path)?;

        // Step 2: Revoke any (now explicit) allow ACE for this SID that was
        // inherited from the parent grant. Without this, the allow would
        // propagate to children alongside the deny, and Windows evaluates
        // inherited ACEs in DACL order (not deny-first).
        apply_ace(sid, path, REVOKE_ACCESS, access_mask)?;
    }

    // Step 3: Add the deny ACE.
    apply_ace(sid, path, DENY_ACCESS, access_mask)?;

    Ok(())
}

/// Set `PROTECTED_DACL_SECURITY_INFORMATION` on a path, blocking inheritance
/// from parent directories. Existing inherited ACEs are converted to explicit.
/// Delegates to `modify_dacl` with an empty entry list and the PROTECTED flag.
fn protect_dacl(path: &Path) -> io::Result<()> {
    let wide_path = path_to_wide(path);
    let display = path.display().to_string();

    modify_dacl(
        &wide_path,
        &display,
        &[],
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
    )
    .map_err(|e| io::Error::other(e.to_string()))
}

// ── Security capabilities ────────────────────────────────────────────

fn create_internet_client_sid() -> io::Result<PSID> {
    let mut sid: PSID = std::ptr::null_mut();

    // SAFETY: Allocating a well-known SID for the internetClient capability (S-1-15-3-1).
    let ret = unsafe {
        AllocateAndInitializeSid(
            &SECURITY_APP_PACKAGE_AUTHORITY,
            SECURITY_BUILTIN_APP_PACKAGE_RID_COUNT,
            SECURITY_CAPABILITY_BASE_RID,
            SECURITY_CAPABILITY_INTERNET_CLIENT,
            0,
            0,
            0,
            0,
            0,
            0,
            &raw mut sid,
        )
    };
    if ret == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(sid)
}

// ── WindowsSandboxedChild ────────────────────────────────────────────

pub struct WindowsSandboxedChild {
    pid: u32,
    process_handle: HANDLE,
    thread_handle: HANDLE,
    _job: JobObject,
    _profile_name: String,
    sentinel: SentinelFile,
    stdin_pipe: Option<std::fs::File>,
    stdout_pipe: Option<std::fs::File>,
    stderr_pipe: Option<std::fs::File>,
    cap_sids: Vec<PSID>,
    app_container_sid: PSID,
}

// SAFETY: All fields are either owned values or HANDLE/PSID (opaque pointer-sized
// integers on Windows). The underlying OS resources can be used from any thread.
// JobObject contains a HANDLE which is also safe to send.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for WindowsSandboxedChild {}
// SAFETY: No interior mutability. All mutation requires &mut self.
unsafe impl Sync for WindowsSandboxedChild {}

impl WindowsSandboxedChild {
    pub const fn id(&self) -> u32 {
        self.pid
    }

    pub fn kill(&mut self) -> io::Result<()> {
        // SAFETY: process_handle is valid from CreateProcessW.
        let ret = unsafe { TerminateProcess(self.process_handle, 1) };
        if ret == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        use std::os::windows::process::ExitStatusExt;

        // SAFETY: process_handle is valid. 0xFFFF_FFFF = INFINITE.
        let wait = unsafe { WaitForSingleObject(self.process_handle, 0xFFFF_FFFF) };
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }

        let mut exit_code: u32 = 0;
        // SAFETY: Process has exited; exit code is available.
        let ret = unsafe { GetExitCodeProcess(self.process_handle, &raw mut exit_code) };
        if ret == FALSE {
            return Err(io::Error::last_os_error());
        }

        Ok(std::process::ExitStatus::from_raw(exit_code))
    }

    pub fn try_wait(&self) -> io::Result<Option<std::process::ExitStatus>> {
        use std::os::windows::process::ExitStatusExt;

        // SAFETY: process_handle is valid. Timeout=0 for non-blocking.
        let wait = unsafe { WaitForSingleObject(self.process_handle, 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }

        let mut exit_code: u32 = 0;
        // SAFETY: Process has exited.
        let ret = unsafe { GetExitCodeProcess(self.process_handle, &raw mut exit_code) };
        if ret == FALSE {
            return Err(io::Error::last_os_error());
        }

        Ok(Some(std::process::ExitStatus::from_raw(exit_code)))
    }

    pub fn wait_with_output(mut self) -> io::Result<std::process::Output> {
        drop(self.stdin_pipe.take());

        // Read stdout and stderr concurrently to avoid deadlock when the
        // child fills one pipe buffer while we block reading the other.
        let stdout_pipe = self.stdout_pipe.take();
        let stderr_pipe = self.stderr_pipe.take();

        let stdout_thread = stdout_pipe.map(|mut f| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                io::Read::read_to_end(&mut f, &mut buf).map(|_| buf)
            })
        });
        let stderr_thread = stderr_pipe.map(|mut f| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                io::Read::read_to_end(&mut f, &mut buf).map(|_| buf)
            })
        });

        let stdout = match stdout_thread {
            Some(t) => t
                .join()
                .map_err(|_| io::Error::other("stdout reader panicked"))?,
            None => Ok(Vec::new()),
        }?;
        let stderr = match stderr_thread {
            Some(t) => t
                .join()
                .map_err(|_| io::Error::other("stderr reader panicked"))?,
            None => Ok(Vec::new()),
        }?;

        let status = self.wait()?;

        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    pub const fn take_stdin(&mut self) -> Option<std::fs::File> {
        self.stdin_pipe.take()
    }

    pub const fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.stdout_pipe.take()
    }

    pub const fn take_stderr(&mut self) -> Option<std::fs::File> {
        self.stderr_pipe.take()
    }

    /// Kill the sandboxed process and all descendants (via the Job Object),
    /// wait for exit, then run platform cleanup (ACL restoration, SID freeing).
    ///
    /// Consumes `self`; Drop still runs but `cleanup()` is idempotent.
    pub fn kill_and_cleanup(mut self) -> crate::Result<()> {
        // TerminateProcess is redundant when the Job has KILL_ON_JOB_CLOSE,
        // but we call it explicitly so descendants die before we wait.
        // kill() may fail with ERROR_ACCESS_DENIED if already exiting — benign.
        let _ = self.kill();
        self.wait().map_err(crate::SandboxError::Io)?;

        // ACL restoration + SID freeing. cleanup() is idempotent so Drop
        // calling it again is harmless.
        self.cleanup();
        Ok(())
    }

    fn cleanup(&mut self) {
        let _ = restore_from_sentinel(&self.sentinel);

        for sid in self.cap_sids.drain(..) {
            // SAFETY: Each sid was allocated by AllocateAndInitializeSid.
            unsafe {
                FreeSid(sid);
            }
        }

        if !self.app_container_sid.is_null() {
            // SAFETY: app_container_sid was allocated by CreateAppContainerProfile.
            unsafe {
                FreeSid(self.app_container_sid);
            }
            self.app_container_sid = std::ptr::null_mut();
        }
    }
}

impl Drop for WindowsSandboxedChild {
    fn drop(&mut self) {
        self.cleanup();

        // SAFETY: Closing valid handles from CreateProcessW.
        unsafe {
            if self.process_handle != INVALID_HANDLE_VALUE {
                CloseHandle(self.process_handle);
            }
            if self.thread_handle != INVALID_HANDLE_VALUE {
                CloseHandle(self.thread_handle);
            }
        }
    }
}

// ── spawn ────────────────────────────────────────────────────────────

/// Spawn a sandboxed process using Windows `AppContainer`.
#[allow(clippy::too_many_lines)]
pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<crate::SandboxedChild> {
    let (profile_name, ac_sid) = create_profile()
        .map_err(|e| SandboxError::Setup(format!("CreateAppContainerProfile: {e}")))?;

    match spawn_inner(policy, command, &profile_name, ac_sid) {
        Ok(child) => Ok(child),
        Err(e) => {
            // SAFETY: ac_sid was allocated by CreateAppContainerProfile.
            unsafe {
                FreeSid(ac_sid);
            }
            let _ = delete_profile(&profile_name);
            Err(e)
        }
    }
}

/// Inner spawn logic. On error, the caller frees `ac_sid` and deletes the profile.
#[allow(clippy::too_many_lines)]
fn spawn_inner(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
    profile_name: &str,
    ac_sid: PSID,
) -> Result<crate::SandboxedChild> {
    // Collect all paths that need ACL modification (grants + denies).
    let all_paths: Vec<PathBuf> = policy
        .read_paths()
        .iter()
        .chain(policy.write_paths().iter())
        .chain(policy.exec_paths().iter())
        .chain(policy.deny_paths().iter())
        .cloned()
        .collect();

    // Grant traverse ACEs on ancestors of policy paths so the sandboxed
    // process can walk path components. Succeeds without elevation for
    // user-owned directories; fails for system directories that require
    // elevated setup via grant_appcontainer_prerequisites().
    let ancestors = super::traverse_acl::compute_ancestors(&all_paths)?;
    let mut prereq_failed: Vec<PathBuf> = Vec::new();
    for ancestor in &ancestors {
        if let Err(e) = super::traverse_acl::grant_traverse(ancestor) {
            // ACCESS_DENIED means elevation is required (PrerequisitesNotMet).
            // Other errors are transient I/O failures propagated as Setup.
            let is_access_denied =
                matches!(&e, SandboxError::Setup(msg) if msg.contains(ELEVATION_REQUIRED_MARKER));
            if is_access_denied {
                prereq_failed.push(ancestor.clone());
            } else {
                return Err(e);
            }
        }
    }
    let nul_missing = !super::nul_device::nul_device_accessible();
    if !prereq_failed.is_empty() || nul_missing {
        return Err(SandboxError::PrerequisitesNotMet {
            missing_paths: prereq_failed,
            nul_device_missing: nul_missing,
        });
    }

    // Write sentinel with original DACLs before modifying anything.
    let sentinel = write_sentinel(profile_name, &all_paths)
        .map_err(|e| SandboxError::Setup(format!("write sentinel: {e}")))?;

    // From here on, errors must restore ACLs via the sentinel.
    match spawn_with_sentinel(policy, command, profile_name, ac_sid, sentinel) {
        Ok(child) => Ok(child),
        Err((sent, err)) => {
            let _ = restore_from_sentinel(&sent);
            Err(err)
        }
    }
}

/// Clean up attr_list and SIDs on error after attr_list has been initialized.
///
/// # Safety
/// `attr_list` must be a valid initialized attribute list. Each SID in
/// `cap_sids` must be a valid SID from `AllocateAndInitializeSid`.
unsafe fn cleanup_attr_and_sids(attr_list: LPPROC_THREAD_ATTRIBUTE_LIST, cap_sids: &[PSID]) {
    // SAFETY: Caller guarantees attr_list is valid.
    unsafe {
        DeleteProcThreadAttributeList(attr_list);
    }
    for sid in cap_sids {
        // SAFETY: Caller guarantees each SID is valid.
        unsafe {
            FreeSid(*sid);
        }
    }
}

/// Spawn after sentinel is written. Returns sentinel back on error for cleanup.
#[allow(clippy::too_many_lines)]
fn spawn_with_sentinel(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
    profile_name: &str,
    ac_sid: PSID,
    sentinel: SentinelFile,
) -> std::result::Result<crate::SandboxedChild, (SentinelFile, SandboxError)> {
    macro_rules! try_setup {
        ($expr:expr, $msg:expr) => {
            match $expr {
                Ok(v) => v,
                Err(e) => return Err((sentinel, SandboxError::Setup(format!("{}: {e}", $msg)))),
            }
        };
    }

    try_setup!(apply_policy_acls(ac_sid, policy), "apply policy ACLs");

    let job = try_setup!(JobObject::new(), "create job object");
    try_setup!(job.set_limits(policy.limits()), "set job limits");

    // Build security capabilities.
    let mut cap_sids: Vec<PSID> = Vec::new();
    let mut capabilities: Vec<SID_AND_ATTRIBUTES> = Vec::new();

    if policy.allow_network() {
        let inet_sid = try_setup!(
            create_internet_client_sid(),
            "create internet capability SID"
        );
        cap_sids.push(inet_sid);
        capabilities.push(SID_AND_ATTRIBUTES {
            Sid: inet_sid,
            Attributes: 4, // SE_GROUP_ENABLED
        });
    }

    let mut sec_caps = SECURITY_CAPABILITIES {
        AppContainerSid: ac_sid,
        Capabilities: if capabilities.is_empty() {
            std::ptr::null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        #[allow(clippy::cast_possible_truncation)]
        CapabilityCount: capabilities.len() as u32,
        Reserved: 0,
    };

    // Create pipes before the attribute list so child handles can be listed
    // in PROC_THREAD_ATTRIBUTE_HANDLE_LIST, eliminating the race window where
    // another thread could inherit handles between CreatePipe and SetHandleInformation.
    let (child_stdin, parent_stdin) = try_setup!(resolve_stdio_input(command.stdin), "stdin pipe");
    let (child_stdout, parent_stdout) =
        match resolve_stdio_output(command.stdout, STD_OUTPUT_HANDLE) {
            Ok(v) => v,
            Err(e) => {
                close_optional_handle(parent_stdin);
                close_handle_if_valid(child_stdin);
                return Err((sentinel, SandboxError::Setup(format!("stdout pipe: {e}"))));
            }
        };
    let (child_stderr, parent_stderr) = match resolve_stdio_output(command.stderr, STD_ERROR_HANDLE)
    {
        Ok(v) => v,
        Err(e) => {
            close_optional_handle(parent_stdin);
            close_handle_if_valid(child_stdin);
            close_optional_handle(parent_stdout);
            close_handle_if_valid(child_stdout);
            return Err((sentinel, SandboxError::Setup(format!("stderr pipe: {e}"))));
        }
    };

    // Collect child-side handles for the explicit handle inheritance list.
    let mut child_handles: Vec<HANDLE> = Vec::new();
    if child_stdin != INVALID_HANDLE_VALUE && !child_stdin.is_null() {
        child_handles.push(child_stdin);
    }
    if child_stdout != INVALID_HANDLE_VALUE && !child_stdout.is_null() {
        child_handles.push(child_stdout);
    }
    if child_stderr != INVALID_HANDLE_VALUE && !child_stderr.is_null() {
        child_handles.push(child_stderr);
    }

    let has_child_handles = !child_handles.is_empty();
    // 1 attribute for security caps, plus 1 for handle list if we have child handles.
    let attr_count: u32 = if has_child_handles { 2 } else { 1 };

    // Cleanup helper for the pipe handles allocated above.
    let close_all_pipes = |ps: Option<HANDLE>,
                           cs: HANDLE,
                           po: Option<HANDLE>,
                           co: HANDLE,
                           pe: Option<HANDLE>,
                           ce: HANDLE| {
        close_optional_handle(ps);
        close_handle_if_valid(cs);
        close_optional_handle(po);
        close_handle_if_valid(co);
        close_optional_handle(pe);
        close_handle_if_valid(ce);
    };

    // Initialize proc thread attribute list.
    let mut attr_list_size: usize = 0;
    // SAFETY: First call to determine required buffer size.
    unsafe {
        InitializeProcThreadAttributeList(
            std::ptr::null_mut(),
            attr_count,
            0,
            &raw mut attr_list_size,
        );
    }

    let mut attr_list_buf: Vec<u8> = vec![0u8; attr_list_size];
    let attr_list: LPPROC_THREAD_ATTRIBUTE_LIST = attr_list_buf.as_mut_ptr().cast();

    // SAFETY: Buffer is correctly sized from the first call.
    let ret = unsafe {
        InitializeProcThreadAttributeList(attr_list, attr_count, 0, &raw mut attr_list_size)
    };
    if ret == FALSE {
        close_all_pipes(
            parent_stdin,
            child_stdin,
            parent_stdout,
            child_stdout,
            parent_stderr,
            child_stderr,
        );
        for sid in &cap_sids {
            // SAFETY: Each sid from AllocateAndInitializeSid.
            unsafe {
                FreeSid(*sid);
            }
        }
        return Err((
            sentinel,
            SandboxError::Setup(format!(
                "InitializeProcThreadAttributeList: {}",
                io::Error::last_os_error()
            )),
        ));
    }

    // SAFETY: attr_list is initialized. sec_caps will live until CreateProcessW returns.
    let ret = unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            (&raw mut sec_caps).cast(),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ret == FALSE {
        let e = io::Error::last_os_error();
        close_all_pipes(
            parent_stdin,
            child_stdin,
            parent_stdout,
            child_stdout,
            parent_stderr,
            child_stderr,
        );
        // SAFETY: attr_list is initialized; SIDs are from AllocateAndInitializeSid.
        unsafe {
            cleanup_attr_and_sids(attr_list, &cap_sids);
        }
        return Err((
            sentinel,
            SandboxError::Setup(format!("UpdateProcThreadAttribute: {e}")),
        ));
    }

    // Add explicit handle inheritance list to prevent handle leaks from racing threads.
    if has_child_handles {
        // SAFETY: child_handles vec lives until after CreateProcessW.
        let ret = unsafe {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                child_handles.as_mut_ptr().cast(),
                child_handles.len() * std::mem::size_of::<HANDLE>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ret == FALSE {
            let e = io::Error::last_os_error();
            close_all_pipes(
                parent_stdin,
                child_stdin,
                parent_stdout,
                child_stdout,
                parent_stderr,
                child_stderr,
            );
            // SAFETY: attr_list is initialized; SIDs are from AllocateAndInitializeSid.
            unsafe {
                cleanup_attr_and_sids(attr_list, &cap_sids);
            }
            return Err((
                sentinel,
                SandboxError::Setup(format!("UpdateProcThreadAttribute(HANDLE_LIST): {e}")),
            ));
        }
    }

    let result = create_sandboxed_process(
        command,
        &job,
        attr_list,
        child_stdin,
        parent_stdin,
        child_stdout,
        parent_stdout,
        child_stderr,
        parent_stderr,
    );

    // SAFETY: attr_list must always be freed.
    unsafe {
        DeleteProcThreadAttributeList(attr_list);
    }

    match result {
        Ok((pi, stdin_file, stdout_file, stderr_file)) => Ok(crate::SandboxedChild {
            inner: WindowsSandboxedChild {
                pid: pi.dwProcessId,
                process_handle: pi.hProcess,
                thread_handle: pi.hThread,
                _job: job,
                _profile_name: profile_name.to_owned(),
                sentinel,
                stdin_pipe: stdin_file,
                stdout_pipe: stdout_file,
                stderr_pipe: stderr_file,
                cap_sids,
                app_container_sid: ac_sid,
            },
        }),
        Err(e) => {
            for sid in &cap_sids {
                // SAFETY: Each sid from AllocateAndInitializeSid.
                unsafe {
                    FreeSid(*sid);
                }
            }
            Err((sentinel, e))
        }
    }
}

/// Create the child process with pre-created pipes and `STARTUPINFOEX`.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
fn create_sandboxed_process(
    command: &SandboxCommand,
    job: &JobObject,
    attr_list: LPPROC_THREAD_ATTRIBUTE_LIST,
    child_stdin: HANDLE,
    parent_stdin: Option<HANDLE>,
    child_stdout: HANDLE,
    parent_stdout: Option<HANDLE>,
    child_stderr: HANDLE,
    parent_stderr: Option<HANDLE>,
) -> std::result::Result<
    (
        PROCESS_INFORMATION,
        Option<std::fs::File>,
        Option<std::fs::File>,
        Option<std::fs::File>,
    ),
    SandboxError,
> {
    // SAFETY: Zeroing a POD struct.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    #[allow(clippy::cast_possible_truncation)]
    {
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    }
    si.lpAttributeList = attr_list;

    let has_stdio = command.stdin != SandboxStdio::Null
        || command.stdout != SandboxStdio::Null
        || command.stderr != SandboxStdio::Null;

    if has_stdio {
        si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = child_stdin;
        si.StartupInfo.hStdOutput = child_stdout;
        si.StartupInfo.hStdError = child_stderr;
    }

    let mut cmd_line = build_command_line(&command.program, &command.args);

    let env_block = if command.env.is_empty() {
        None
    } else {
        Some(build_env_block(&command.env))
    };

    let cwd_wide = command.cwd.as_ref().map(|p| path_to_wide(p));

    let mut creation_flags = EXTENDED_STARTUPINFO_PRESENT;
    if env_block.is_some() {
        creation_flags |= CREATE_UNICODE_ENVIRONMENT;
    }

    let inherit_handles = if has_stdio { TRUE } else { FALSE };

    // SAFETY: Zeroing a POD struct.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // SAFETY: All pointers are valid for the duration of this call.
    let ret = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            inherit_handles,
            creation_flags,
            env_block
                .as_ref()
                .map_or(std::ptr::null(), |b| b.as_ptr().cast()),
            cwd_wide.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
            (&raw const si.StartupInfo),
            &raw mut pi,
        )
    };

    // Close child-side pipe handles in the parent after CreateProcessW.
    // The child inherited these handles via PROC_THREAD_ATTRIBUTE_HANDLE_LIST.
    // Only Piped streams have parent-owned child handles; Null and Inherit
    // handles are either system-owned (NUL device) or the parent's own
    // console handles, which must not be closed here.
    if command.stdin == SandboxStdio::Piped {
        close_handle_if_valid(child_stdin);
    }
    if command.stdout == SandboxStdio::Piped {
        close_handle_if_valid(child_stdout);
    }
    if command.stderr == SandboxStdio::Piped {
        close_handle_if_valid(child_stderr);
    }

    if ret == FALSE {
        let err = io::Error::last_os_error();
        close_optional_handle(parent_stdin);
        close_optional_handle(parent_stdout);
        close_optional_handle(parent_stderr);
        return Err(SandboxError::Setup(format!("CreateProcessW: {err}")));
    }

    // Assign to job.
    if let Err(e) = job.assign_process(pi.hProcess) {
        // SAFETY: Terminate the unsandboxed process.
        unsafe {
            TerminateProcess(pi.hProcess, 1);
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
        }
        close_optional_handle(parent_stdin);
        close_optional_handle(parent_stdout);
        close_optional_handle(parent_stderr);
        return Err(SandboxError::Setup(format!("assign to job: {e}")));
    }

    // SAFETY: Each handle is valid from CreatePipe, ownership transfers to File.
    let stdin_file = parent_stdin.map(|h| unsafe { std::fs::File::from_raw_handle(h.cast()) });
    let stdout_file = parent_stdout.map(|h| unsafe { std::fs::File::from_raw_handle(h.cast()) });
    let stderr_file = parent_stderr.map(|h| unsafe { std::fs::File::from_raw_handle(h.cast()) });

    Ok((pi, stdin_file, stdout_file, stderr_file))
}

fn apply_policy_acls(sid: PSID, policy: &SandboxPolicy) -> io::Result<()> {
    for path in policy.read_paths() {
        grant_access(sid, path, false)?;
    }
    for path in policy.write_paths() {
        grant_access(sid, path, true)?;
    }
    for path in policy.exec_paths() {
        grant_access(sid, path, false)?;
    }
    // Deny ACEs are evaluated before allow ACEs by Windows, so these
    // override any inherited allows from parent grant paths.
    for path in policy.deny_paths() {
        deny_all_file_access(sid, path)?;
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::policy::{ResourceLimits, SandboxPolicy};
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    use super::super::sentinel::get_sddl;

    // Serialize appcontainer tests to prevent cleanup_stale from interfering
    // with concurrent test sessions that share the same temp directory.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Create temp dir inside the project to avoid system temp ancestors
    /// (e.g. `C:\Users`) that require elevation for traverse ACE grants.
    fn make_temp_dir() -> TempDir {
        let test_tmp = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
        TempDir::new_in(&test_tmp).expect("create temp dir")
    }

    /// Set sandbox-safe overrides for path-bearing vars, then forward
    /// remaining parent env. `forward_common_env` skips already-set keys,
    /// so the overrides take priority.
    fn set_sandbox_env(cmd: &mut SandboxCommand, scratch: &Path) {
        let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
        let sys32 = format!(r"{sys_root}\System32");
        cmd.env("PATH", &sys32);
        cmd.env("TEMP", scratch);
        cmd.env("TMP", scratch);
        cmd.env("TMPDIR", scratch);
        cmd.forward_common_env();
    }

    /// Spawn, returning None if prerequisites aren't met (non-elevated env).
    fn must_spawn(policy: &SandboxPolicy, cmd: &SandboxCommand) -> crate::SandboxedChild {
        crate::spawn(policy, cmd).expect("spawn must succeed")
    }

    /// Strip DACL control flags (like AI, P) from SDDL for comparison.
    /// `SetNamedSecurityInfoW` recalculates auto-inherit flags, so exact SDDL
    /// comparison would fail. Comparing only the ACE entries is sufficient.
    fn normalize_sddl(sddl: &str) -> String {
        sddl.find("D:").map_or_else(
            || sddl.to_owned(),
            |d_pos| {
                let after_d = &sddl[d_pos + 2..];
                after_d.find('(').map_or_else(
                    || "D:".to_owned(),
                    |paren_pos| format!("D:{}", &after_d[paren_pos..]),
                )
            },
        )
    }

    /// Extract only explicit (non-inherited) ACEs from an SDDL string.
    /// Inherited ACEs have the `ID` flag and may change when ancestor
    /// DACLs are modified without inheritance propagation.
    fn explicit_aces(sddl: &str) -> String {
        let normalized = normalize_sddl(sddl);
        let mut result = String::from("D:");
        // Each ACE is a parenthesized group.
        for ace in normalized.split('(').skip(1) {
            if let Some(ace_content) = ace.strip_suffix(')') {
                // ACE format: (type;flags;rights;...;trustee)
                // The `ID` flag in the flags field indicates inherited.
                let parts: Vec<&str> = ace_content.split(';').collect();
                if parts.len() >= 2 && !parts[1].contains("ID") {
                    result.push('(');
                    result.push_str(ace_content);
                    result.push(')');
                }
            }
        }
        result
    }

    fn write_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).expect("write test file");
        p
    }

    #[test]
    fn spawn_and_read_allowed_path() {
        let _guard = TEST_LOCK.lock().unwrap();
        let tmp = make_temp_dir();
        let scratch = make_temp_dir();
        let file = write_test_file(tmp.path(), "hello.txt", "sandbox_test_content_42");

        let policy = SandboxPolicy::new(
            vec![tmp.path().to_path_buf()],
            vec![scratch.path().to_path_buf()],
            Vec::new(),
            Vec::new(),
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("cmd.exe");
        set_sandbox_env(&mut cmd, scratch.path());
        cmd.args(["/C", "type"]);
        cmd.arg(file.as_os_str());

        let child = must_spawn(&policy, &cmd);
        let output = child.wait_with_output().expect("wait_with_output");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("sandbox_test_content_42"),
            "stdout should contain file content, got: {stdout}"
        );
    }

    #[test]
    fn disallowed_path_unreadable() {
        let _guard = TEST_LOCK.lock().unwrap();
        let allowed = make_temp_dir();
        let scratch = make_temp_dir();
        let forbidden = make_temp_dir();
        let file = write_test_file(forbidden.path(), "secret.txt", "secret_data");

        let policy = SandboxPolicy::new(
            vec![allowed.path().to_path_buf()],
            vec![scratch.path().to_path_buf()],
            Vec::new(),
            Vec::new(),
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("cmd.exe");
        set_sandbox_env(&mut cmd, scratch.path());
        cmd.args(["/C", "type"]);
        cmd.arg(file.as_os_str());

        let child = must_spawn(&policy, &cmd);
        let output = child.wait_with_output().expect("wait");

        assert!(
            !output.status.success(),
            "process should fail when reading disallowed path"
        );
    }

    #[test]
    fn read_only_path_not_writable() {
        let _guard = TEST_LOCK.lock().unwrap();
        let tmp = make_temp_dir();
        let scratch = make_temp_dir();
        let target = tmp.path().join("readonly_test.txt");
        fs::write(&target, "original").expect("write");

        let policy = SandboxPolicy::new(
            vec![tmp.path().to_path_buf()],
            vec![scratch.path().to_path_buf()],
            Vec::new(),
            Vec::new(),
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("cmd.exe");
        set_sandbox_env(&mut cmd, scratch.path());
        // Use short path form without spaces by writing to a file in a temp dir.
        // The redirect must be in a single /C argument so cmd.exe interprets `>`.
        let target_str = target.to_string_lossy();
        cmd.args(["/C", &format!("echo overwritten > {target_str}")]);

        let child = must_spawn(&policy, &cmd);
        let output = child.wait_with_output().expect("wait");

        let content = fs::read_to_string(&target).expect("read back");
        if output.status.success() {
            assert_eq!(content.trim(), "original", "file should not be overwritten");
        }
    }

    #[test]
    fn cleanup_restores_acls() {
        let _guard = TEST_LOCK.lock().unwrap();
        let tmp = make_temp_dir();
        let scratch = make_temp_dir();
        let _file = write_test_file(tmp.path(), "acl_test.txt", "test");

        let original_sddl = get_sddl(tmp.path()).expect("get original SDDL");

        let policy = SandboxPolicy::new(
            vec![tmp.path().to_path_buf()],
            vec![scratch.path().to_path_buf()],
            Vec::new(),
            Vec::new(),
            false,
            ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("cmd.exe");
        set_sandbox_env(&mut cmd, scratch.path());
        cmd.args(["/C", "echo done"]);

        {
            let child = must_spawn(&policy, &cmd);
            let _ = child.wait_with_output();
        }

        let restored_sddl = get_sddl(tmp.path()).expect("get restored SDDL");
        // Compare only explicit ACEs. Inherited ACEs may differ when ancestor
        // DACLs are modified by grant_traverse (which uses NtSetSecurityObject
        // without inheritance propagation). The security guarantee is that
        // explicit ACEs — including any AppContainer grants — are fully restored.
        assert_eq!(
            explicit_aces(&original_sddl),
            explicit_aces(&restored_sddl),
            "explicit DACL ACEs should be restored after child exits"
        );
    }

    #[test]
    fn sentinel_recovery() {
        let _guard = TEST_LOCK.lock().unwrap();
        let tmp = make_temp_dir();
        let _file = write_test_file(tmp.path(), "sentinel_test.txt", "test");

        let original_sddl = get_sddl(tmp.path()).expect("get original SDDL");

        let (name, sid) = create_profile().expect("create profile");
        let mut sentinel = SentinelFile::new(name.clone());
        sentinel.add_entry(tmp.path().to_path_buf(), original_sddl.clone());
        sentinel.write().expect("write sentinel");

        grant_access(sid, tmp.path(), false).expect("grant access");

        // SAFETY: sid was allocated by CreateAppContainerProfile.
        unsafe {
            FreeSid(sid);
        }

        let modified_sddl = get_sddl(tmp.path()).expect("get modified SDDL");
        assert_ne!(original_sddl, modified_sddl, "ACL should be modified");

        // Call restore_from_sentinel directly rather than cleanup_stale.
        // cleanup_stale skips sentinels owned by live processes (this PID),
        // and this test is about the restore logic, not the scanning.
        let sentinel_file_path = std::env::temp_dir().join(format!("lot-sentinel-{name}.txt"));
        let sentinel_read = SentinelFile::read(&sentinel_file_path).expect("read sentinel");
        restore_from_sentinel(&sentinel_read).expect("restore_from_sentinel");

        let restored_sddl = get_sddl(tmp.path()).expect("get restored SDDL");
        assert_eq!(
            normalize_sddl(&original_sddl),
            normalize_sddl(&restored_sddl),
            "DACL ACEs should be restored by restore_from_sentinel"
        );

        assert!(
            !sentinel_file_path.exists(),
            "sentinel file should be deleted"
        );
    }
}
