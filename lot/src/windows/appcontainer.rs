#![allow(unsafe_code)]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    BOOL, CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, FALSE, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW,
    NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    ACL, AllocateAndInitializeSid, DACL_SECURITY_INFORMATION, FreeSid, GetSecurityDescriptorDacl,
    OBJECT_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcessId, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::core::HRESULT;

use crate::Result;
use crate::command::{SandboxCommand, SandboxStdio};
use crate::error::SandboxError;
use crate::policy::SandboxPolicy;

use super::job::JobObject;

// ── Constants ────────────────────────────────────────────────────────

const SECURITY_APP_PACKAGE_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 15],
};
const SECURITY_CAPABILITY_INTERNET_CLIENT: u32 = 1;
const SECURITY_BUILTIN_APP_PACKAGE_RID_COUNT: u8 = 2;
const SECURITY_CAPABILITY_BASE_RID: u32 = 3;

use super::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE};

/// Check whether `AppContainer` is available on this Windows version.
pub const fn available() -> bool {
    true
}

// ── Helpers ──────────────────────────────────────────────────────────

fn os_to_wide(s: &OsString) -> Vec<u16> {
    s.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

use super::{path_to_wide, to_wide};

fn unique_profile_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    // SAFETY: Side-effect-free queries returning process ID and tick count.
    let pid = unsafe { GetCurrentProcessId() };
    let tick = unsafe { GetTickCount64() };
    format!("lot-{pid}-{tick}-{seq}")
}

fn hresult_to_io(hr: HRESULT) -> io::Error {
    io::Error::from_raw_os_error(hr)
}

#[allow(clippy::cast_possible_wrap)]
fn win32_to_io(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

fn sentinel_dir() -> PathBuf {
    std::env::temp_dir()
}

fn sentinel_path(profile_name: &str) -> PathBuf {
    sentinel_dir().join(format!("lot-sentinel-{profile_name}.txt"))
}

/// Extract the PID from a profile name. Format: `lot-{pid}-{tick}-{seq}`.
fn pid_from_profile_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("lot-")?;
    let pid_str = rest.split('-').next()?;
    pid_str.parse().ok()
}

/// Check whether a process with the given PID is still running.
fn is_process_alive(pid: u32) -> bool {
    // SAFETY: Querying process existence. Handle closed immediately.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: Handle from OpenProcess.
    unsafe {
        CloseHandle(handle);
    }
    true
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

fn delete_profile(name: &str) -> io::Result<()> {
    let wide = to_wide(name);
    // SAFETY: Valid null-terminated wide string.
    let hr = unsafe { DeleteAppContainerProfile(wide.as_ptr()) };
    if hr != 0 {
        return Err(hresult_to_io(hr));
    }
    Ok(())
}

// ── ACL management ───────────────────────────────────────────────────

fn get_sddl(path: &Path) -> io::Result<String> {
    let wide_path = path_to_wide(path);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reads the DACL of the named object. sd must be freed with `LocalFree`.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(win32_to_io(err));
    }

    let sddl = sd_to_sddl(sd, DACL_SECURITY_INFORMATION)?;

    // SAFETY: sd was allocated by `GetNamedSecurityInfoW`.
    unsafe {
        LocalFree(sd.cast());
    }

    Ok(sddl)
}

fn sd_to_sddl(sd: PSECURITY_DESCRIPTOR, info: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
    let mut sddl_ptr: *mut u16 = std::ptr::null_mut();
    let mut sddl_len: u32 = 0;

    // SAFETY: sd is a valid security descriptor. Output pointer freed with `LocalFree`.
    let ret = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            info,
            &raw mut sddl_ptr,
            &raw mut sddl_len,
        )
    };
    if ret == FALSE {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: sddl_ptr points to a valid wide string of sddl_len chars (including null terminator).
    // Subtract 1 to exclude the null terminator from the slice.
    let len = (sddl_len as usize).saturating_sub(1);
    let sddl_slice = unsafe { std::slice::from_raw_parts(sddl_ptr, len) };
    let sddl = String::from_utf16_lossy(sddl_slice);

    // SAFETY: sddl_ptr was allocated by the conversion function.
    unsafe {
        LocalFree(sddl_ptr.cast());
    }

    Ok(sddl)
}

fn restore_sddl(path: &Path, sddl: &str) -> io::Result<()> {
    let wide_sddl = to_wide(sddl);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Converts a valid SDDL string to a security descriptor.
    let ret = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut sd,
            std::ptr::null_mut(),
        )
    };
    if ret == FALSE {
        return Err(io::Error::last_os_error());
    }

    let mut dacl_present: BOOL = FALSE;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut dacl_defaulted: BOOL = FALSE;

    // SAFETY: sd is a valid security descriptor from the conversion above.
    let ret = unsafe {
        GetSecurityDescriptorDacl(
            sd,
            &raw mut dacl_present,
            &raw mut dacl,
            &raw mut dacl_defaulted,
        )
    };
    if ret == FALSE {
        unsafe {
            LocalFree(sd.cast());
        }
        return Err(io::Error::last_os_error());
    }

    let dacl_to_set = if dacl_present == FALSE {
        std::ptr::null_mut()
    } else {
        dacl
    };

    let wide_path = path_to_wide(path);

    // SAFETY: Setting DACL on a named object. Owner/group/SACL unchanged (null).
    // Use DACL_SECURITY_INFORMATION alone to replace the DACL entirely,
    // preventing re-inheritance of ACEs from the parent.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl_to_set,
            std::ptr::null(),
        )
    };

    // SAFETY: sd was allocated by the conversion function.
    unsafe {
        LocalFree(sd.cast());
    }

    if err != ERROR_SUCCESS {
        return Err(win32_to_io(err));
    }
    Ok(())
}

fn grant_access(sid: PSID, path: &Path, writable: bool) -> io::Result<()> {
    let wide_path = path_to_wide(path);
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading current DACL of the file/directory.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut current_dacl,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(win32_to_io(err));
    }

    let access_mask = if writable {
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE
    } else {
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
    };

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
        grfAccessMode: SET_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: trustee,
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging a new ACE into the existing DACL.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, current_dacl, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        unsafe {
            LocalFree(sd.cast());
        }
        return Err(win32_to_io(err));
    }

    // SAFETY: Applying the new DACL. Owner/group/SACL unchanged.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null(),
        )
    };

    // SAFETY: new_dacl from SetEntriesInAclW, sd from GetNamedSecurityInfoW.
    unsafe {
        LocalFree(new_dacl.cast());
        LocalFree(sd.cast());
    }

    if err != ERROR_SUCCESS {
        return Err(win32_to_io(err));
    }
    Ok(())
}

// ── Sentinel file ────────────────────────────────────────────────────

/// Line 1: profile name. Subsequent lines: `path\tSDDL`.
struct SentinelFile {
    profile_name: String,
    entries: Vec<(PathBuf, String)>,
}

impl SentinelFile {
    const fn new(profile_name: String) -> Self {
        Self {
            profile_name,
            entries: Vec::new(),
        }
    }

    fn add_entry(&mut self, path: PathBuf, sddl: String) {
        self.entries.push((path, sddl));
    }

    fn write(&self) -> io::Result<()> {
        let path = sentinel_path(&self.profile_name);
        let mut content = self.profile_name.clone();
        content.push('\n');
        for (p, sddl) in &self.entries {
            content.push_str(&Self::encode_path_field(&p.to_string_lossy()));
            content.push('\t');
            content.push_str(sddl);
            content.push('\n');
        }
        fs::write(path, content)
    }

    /// Percent-encode tab, newline, carriage return, and percent in the path field
    /// so they don't break the `path\tSDDL\n` line format.
    fn encode_path_field(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '%' => out.push_str("%25"),
                '\t' => out.push_str("%09"),
                '\n' => out.push_str("%0A"),
                '\r' => out.push_str("%0D"),
                other => out.push(other),
            }
        }
        out
    }

    fn decode_path_field(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                let hex: String = chars.by_ref().take(2).collect();
                match hex.as_str() {
                    "25" => out.push('%'),
                    "09" => out.push('\t'),
                    "0A" => out.push('\n'),
                    "0D" => out.push('\r'),
                    _ => {
                        out.push('%');
                        out.push_str(&hex);
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn read(sentinel: &Path) -> io::Result<Self> {
        let content = fs::read_to_string(sentinel)?;
        let mut lines = content.lines();
        let profile_name = lines
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty sentinel file"))?
            .to_owned();

        let mut entries = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((path_str, sddl)) = line.split_once('\t') {
                entries.push((
                    PathBuf::from(Self::decode_path_field(path_str)),
                    sddl.to_owned(),
                ));
            }
        }
        Ok(Self {
            profile_name,
            entries,
        })
    }

    fn delete_file(&self) -> io::Result<()> {
        let path = sentinel_path(&self.profile_name);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn write_sentinel(profile_name: &str, paths: &[PathBuf]) -> io::Result<SentinelFile> {
    let mut sentinel = SentinelFile::new(profile_name.to_owned());
    for path in paths {
        let sddl = get_sddl(path)?;
        sentinel.add_entry(path.clone(), sddl);
    }
    sentinel.write()?;
    Ok(sentinel)
}

fn restore_from_sentinel(sentinel: &SentinelFile) -> Result<()> {
    let mut errors = Vec::new();

    for (path, sddl) in &sentinel.entries {
        if let Err(e) = restore_sddl(path, sddl) {
            errors.push(format!("{}: {e}", path.display()));
        }
    }

    if let Err(e) = delete_profile(&sentinel.profile_name) {
        errors.push(format!("delete profile {}: {e}", sentinel.profile_name));
    }

    if let Err(e) = sentinel.delete_file() {
        errors.push(format!("delete sentinel: {e}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SandboxError::Cleanup(errors.join("; ")))
    }
}

/// Restore ACLs from stale sentinel files left by crashed sessions.
pub fn cleanup_stale() -> Result<()> {
    let dir = sentinel_dir();
    let mut errors = Vec::new();

    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(());
    };

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("lot-sentinel-")
            || !std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        {
            continue;
        }
        match SentinelFile::read(&path) {
            Ok(sentinel) => {
                // Skip sentinels owned by a still-running process — they
                // are live, not stale. The owning process's Drop will
                // clean them up.
                if let Some(pid) = pid_from_profile_name(&sentinel.profile_name) {
                    if is_process_alive(pid) {
                        continue;
                    }
                }
                if let Err(e) = restore_from_sentinel(&sentinel) {
                    errors.push(format!("{}: {e}", path.display()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Sentinel was cleaned up concurrently; skip it.
            }
            Err(e) => {
                errors.push(format!("{}: {e}", path.display()));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SandboxError::Cleanup(errors.join("; ")))
    }
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

// ── Pipe helpers ─────────────────────────────────────────────────────

struct PipeHandles {
    read: HANDLE,
    write: HANDLE,
}

fn create_pipe() -> io::Result<PipeHandles> {
    #[allow(clippy::cast_possible_truncation)]
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };

    let mut read_handle: HANDLE = INVALID_HANDLE_VALUE;
    let mut write_handle: HANDLE = INVALID_HANDLE_VALUE;

    // SAFETY: Creating an anonymous pipe with inheritable handles.
    // Both handles are inheritable, but PROC_THREAD_ATTRIBUTE_HANDLE_LIST
    // restricts which handles the child actually inherits.
    let ret = unsafe { CreatePipe(&raw mut read_handle, &raw mut write_handle, &raw mut sa, 0) };
    if ret == FALSE {
        return Err(io::Error::last_os_error());
    }

    Ok(PipeHandles {
        read: read_handle,
        write: write_handle,
    })
}

fn resolve_stdio_input(spec: SandboxStdio) -> io::Result<(HANDLE, Option<HANDLE>)> {
    match spec {
        SandboxStdio::Null => Ok((INVALID_HANDLE_VALUE, None)),
        SandboxStdio::Inherit => {
            // SAFETY: Querying the current process's stdin handle.
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            Ok((handle, None))
        }
        SandboxStdio::Piped => {
            let pipe = create_pipe()?;
            // Child reads from read end, parent writes to write end.
            Ok((pipe.read, Some(pipe.write)))
        }
    }
}

fn resolve_stdio_output(
    spec: SandboxStdio,
    std_handle_id: u32,
) -> io::Result<(HANDLE, Option<HANDLE>)> {
    match spec {
        SandboxStdio::Null => Ok((INVALID_HANDLE_VALUE, None)),
        SandboxStdio::Inherit => {
            // SAFETY: Querying the current process's stdout/stderr handle.
            let handle = unsafe { GetStdHandle(std_handle_id) };
            Ok((handle, None))
        }
        SandboxStdio::Piped => {
            let pipe = create_pipe()?;
            // Child writes to write end, parent reads from read end.
            Ok((pipe.write, Some(pipe.read)))
        }
    }
}

// ── Process creation helpers ─────────────────────────────────────────

fn build_env_block(env: &[(OsString, OsString)]) -> Vec<u16> {
    let mut block = Vec::new();
    for (k, v) in env {
        block.extend(k.as_os_str().encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.as_os_str().encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn build_command_line(program: &OsString, args: &[OsString]) -> Vec<u16> {
    let mut cmd = OsString::new();
    cmd.push("\"");
    cmd.push(program);
    cmd.push("\"");
    for arg in args {
        cmd.push(" ");
        append_escaped_arg(&mut cmd, arg);
    }
    os_to_wide(&cmd)
}

/// Escape an argument following `CommandLineToArgvW` rules:
/// - If the arg contains spaces, tabs, or quotes, wrap in quotes.
/// - Inside quotes, backslashes before a quote must be doubled, and quotes become `\"`.
fn append_escaped_arg(cmd: &mut OsString, arg: &OsString) {
    let s = arg.to_string_lossy();
    let needs_quoting = s.is_empty() || s.contains(' ') || s.contains('\t') || s.contains('"');

    if !needs_quoting {
        cmd.push(arg);
        return;
    }

    cmd.push("\"");

    let mut backslash_count: usize = 0;
    for ch in s.chars() {
        if ch == '\\' {
            backslash_count += 1;
        } else if ch == '"' {
            // Double the backslashes before a quote, then emit \"
            for _ in 0..backslash_count {
                cmd.push("\\");
            }
            backslash_count = 0;
            cmd.push("\\\"");
        } else {
            backslash_count = 0;
            let mut buf = [0u8; 4];
            cmd.push(ch.encode_utf8(&mut buf) as &str);
        }
    }
    // Double trailing backslashes before the closing quote
    for _ in 0..backslash_count {
        cmd.push("\\");
    }

    cmd.push("\"");
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

    pub fn kill(&self) -> io::Result<()> {
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
    // Collect all paths that need ACL modification.
    let all_paths: Vec<PathBuf> = policy
        .read_paths
        .iter()
        .chain(policy.write_paths.iter())
        .chain(policy.exec_paths.iter())
        .cloned()
        .collect();

    // Grant traverse ACEs on ancestors of policy paths so the sandboxed
    // process can walk path components. Succeeds without elevation for
    // user-owned directories; fails for system directories that require
    // elevated setup via grant_appcontainer_prerequisites().
    let ancestors = super::traverse_acl::compute_ancestors(&all_paths);
    let mut failed: Vec<PathBuf> = Vec::new();
    for ancestor in &ancestors {
        if super::traverse_acl::grant_traverse(ancestor).is_err() {
            failed.push(ancestor.clone());
        }
    }
    let nul_missing = !super::nul_device::nul_device_accessible();
    if !failed.is_empty() || nul_missing {
        return Err(SandboxError::PrerequisitesNotMet {
            missing_paths: failed,
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

    try_setup!(grant_acls(ac_sid, policy), "grant ACLs");

    let job = try_setup!(JobObject::new(), "create job object");
    try_setup!(job.set_limits(&policy.limits), "set job limits");

    // Build security capabilities.
    let mut cap_sids: Vec<PSID> = Vec::new();
    let mut capabilities: Vec<SID_AND_ATTRIBUTES> = Vec::new();

    if policy.allow_network {
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
        close_optional_handle(parent_stdin);
        close_handle_if_valid(child_stdin);
        close_optional_handle(parent_stdout);
        close_handle_if_valid(child_stdout);
        close_optional_handle(parent_stderr);
        close_handle_if_valid(child_stderr);
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
        close_optional_handle(parent_stdin);
        close_handle_if_valid(child_stdin);
        close_optional_handle(parent_stdout);
        close_handle_if_valid(child_stdout);
        close_optional_handle(parent_stderr);
        close_handle_if_valid(child_stderr);
        // SAFETY: Cleaning up initialized attr_list and SIDs.
        unsafe {
            DeleteProcThreadAttributeList(attr_list);
            for sid in &cap_sids {
                FreeSid(*sid);
            }
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
            close_optional_handle(parent_stdin);
            close_handle_if_valid(child_stdin);
            close_optional_handle(parent_stdout);
            close_handle_if_valid(child_stdout);
            close_optional_handle(parent_stderr);
            close_handle_if_valid(child_stderr);
            // SAFETY: Cleaning up.
            unsafe {
                DeleteProcThreadAttributeList(attr_list);
                for sid in &cap_sids {
                    FreeSid(*sid);
                }
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

    // Close child-side pipe handles — the child inherited them.
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

fn close_handle_if_valid(h: HANDLE) {
    if h != INVALID_HANDLE_VALUE && !h.is_null() {
        // SAFETY: Closing a valid handle.
        unsafe {
            CloseHandle(h);
        }
    }
}

fn close_optional_handle(h: Option<HANDLE>) {
    if let Some(handle) = h {
        close_handle_if_valid(handle);
    }
}

fn grant_acls(sid: PSID, policy: &SandboxPolicy) -> io::Result<()> {
    for path in &policy.read_paths {
        grant_access(sid, path, false)?;
    }
    for path in &policy.write_paths {
        grant_access(sid, path, true)?;
    }
    for path in &policy.exec_paths {
        grant_access(sid, path, false)?;
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::policy::{ResourceLimits, SandboxPolicy};
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serialize appcontainer tests to prevent cleanup_stale from interfering
    // with concurrent test sessions that share the same temp directory.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn make_temp_dir() -> TempDir {
        TempDir::new().expect("create temp dir")
    }

    /// Spawn, returning None if prerequisites aren't met (non-elevated env).
    fn try_spawn(policy: &SandboxPolicy, cmd: &SandboxCommand) -> Option<crate::SandboxedChild> {
        match crate::spawn(policy, cmd) {
            Ok(child) => Some(child),
            Err(SandboxError::PrerequisitesNotMet { .. }) => None,
            Err(e) => panic!("unexpected spawn error: {e}"),
        }
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

    fn write_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).expect("write test file");
        p
    }

    #[test]
    fn spawn_and_read_allowed_path() {
        let _guard = TEST_LOCK.lock().unwrap();
        let tmp = make_temp_dir();
        let file = write_test_file(tmp.path(), "hello.txt", "sandbox_test_content_42");

        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };

        let mut cmd = SandboxCommand::new("cmd.exe");
        cmd.args(["/C", "type"]);
        cmd.arg(file.as_os_str());

        let Some(child) = try_spawn(&policy, &cmd) else {
            return; // prerequisites not met (non-elevated)
        };
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
        let forbidden = make_temp_dir();
        let file = write_test_file(forbidden.path(), "secret.txt", "secret_data");

        let policy = SandboxPolicy {
            read_paths: vec![allowed.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };

        let mut cmd = SandboxCommand::new("cmd.exe");
        cmd.args(["/C", "type"]);
        cmd.arg(file.as_os_str());

        let Some(child) = try_spawn(&policy, &cmd) else {
            return; // prerequisites not met (non-elevated)
        };
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
        let target = tmp.path().join("readonly_test.txt");
        fs::write(&target, "original").expect("write");

        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };

        let mut cmd = SandboxCommand::new("cmd.exe");
        // Use short path form without spaces by writing to a file in a temp dir.
        // The redirect must be in a single /C argument so cmd.exe interprets `>`.
        let target_str = target.to_string_lossy();
        cmd.args(["/C", &format!("echo overwritten > {target_str}")]);

        let Some(child) = try_spawn(&policy, &cmd) else {
            return; // prerequisites not met (non-elevated)
        };
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
        let _file = write_test_file(tmp.path(), "acl_test.txt", "test");

        let original_sddl = get_sddl(tmp.path()).expect("get original SDDL");

        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: Vec::new(),
            exec_paths: Vec::new(),
            allow_network: false,
            limits: ResourceLimits::default(),
        };

        let mut cmd = SandboxCommand::new("cmd.exe");
        cmd.args(["/C", "echo done"]);

        {
            let Some(child) = try_spawn(&policy, &cmd) else {
                return; // prerequisites not met (non-elevated)
            };
            let _ = child.wait_with_output();
        }

        let restored_sddl = get_sddl(tmp.path()).expect("get restored SDDL");
        assert_eq!(
            normalize_sddl(&original_sddl),
            normalize_sddl(&restored_sddl),
            "DACL ACEs should be restored after child exits"
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
        let sentinel_read = SentinelFile::read(&sentinel_path(&name)).expect("read sentinel");
        restore_from_sentinel(&sentinel_read).expect("restore_from_sentinel");

        let restored_sddl = get_sddl(tmp.path()).expect("get restored SDDL");
        assert_eq!(
            normalize_sddl(&original_sddl),
            normalize_sddl(&restored_sddl),
            "DACL ACEs should be restored by restore_from_sentinel"
        );

        assert!(
            !sentinel_path(&name).exists(),
            "sentinel file should be deleted"
        );
    }
}
