#![allow(unsafe_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, OBJECT_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::Result;
use crate::error::SandboxError;

use super::{path_to_wide, to_wide};

// ── Helpers ──────────────────────────────────────────────────────────

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

/// Extract the PID from a profile name. Format: `lot-{pid}-{tick}-{qpc}-{seq}`.
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

// ── SDDL helpers ─────────────────────────────────────────────────────

pub fn get_sddl(path: &Path) -> io::Result<String> {
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

/// Convert a security descriptor to an SDDL string.
///
/// Measures string length by walking the null terminator rather than
/// relying on the `sddl_len` output parameter, which has ambiguous
/// documentation regarding null-terminator inclusion.
fn sd_to_sddl(sd: PSECURITY_DESCRIPTOR, info: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
    let mut sddl_ptr: *mut u16 = std::ptr::null_mut();

    // SAFETY: sd is a valid security descriptor. Output pointer freed with `LocalFree`.
    let ret = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            info,
            &raw mut sddl_ptr,
            std::ptr::null_mut(),
        )
    };
    if ret == FALSE {
        return Err(io::Error::last_os_error());
    }

    // Walk the null-terminated wide string to find its length.
    // SAFETY: sddl_ptr is a null-terminated wide string allocated by the conversion function.
    let len = unsafe {
        let mut p = sddl_ptr;
        while *p != 0 {
            p = p.add(1);
        }
        p.offset_from(sddl_ptr) as usize
    };
    let sddl_slice = unsafe { std::slice::from_raw_parts(sddl_ptr, len) };
    let sddl = String::from_utf16_lossy(sddl_slice);

    // SAFETY: sddl_ptr allocated by the conversion function.
    unsafe {
        LocalFree(sddl_ptr.cast());
    }

    Ok(sddl)
}

pub fn restore_sddl(path: &Path, sddl: &str) -> io::Result<()> {
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

    let mut dacl_present: i32 = FALSE;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut dacl_defaulted: i32 = FALSE;

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

// ── Sentinel file ────────────────────────────────────────────────────

/// Line 1: profile name. Subsequent lines: `path\tSDDL`.
/// Paths are percent-encoded: tab (`%09`), newline (`%0A`), carriage return (`%0D`),
/// and percent (`%25`) characters are encoded to preserve the line format.
pub struct SentinelFile {
    pub profile_name: String,
    pub entries: Vec<(PathBuf, String)>,
}

impl SentinelFile {
    pub const fn new(profile_name: String) -> Self {
        Self {
            profile_name,
            entries: Vec::new(),
        }
    }

    pub fn add_entry(&mut self, path: PathBuf, sddl: String) {
        self.entries.push((path, sddl));
    }

    pub fn write(&self) -> io::Result<()> {
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

    pub fn read(sentinel: &Path) -> io::Result<Self> {
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

    pub fn delete_file(&self) -> io::Result<()> {
        let path = sentinel_path(&self.profile_name);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

pub fn write_sentinel(profile_name: &str, paths: &[PathBuf]) -> io::Result<SentinelFile> {
    let mut sentinel = SentinelFile::new(profile_name.to_owned());
    for path in paths {
        let sddl = get_sddl(path)?;
        sentinel.add_entry(path.clone(), sddl);
    }
    sentinel.write()?;
    Ok(sentinel)
}

pub fn restore_from_sentinel(sentinel: &SentinelFile) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for (path, sddl) in &sentinel.entries {
        if let Err(e) = restore_sddl(path, sddl) {
            errors.push(format!("{}: {e}", path.display()));
        }
    }

    if let Err(e) = super::appcontainer::delete_profile(&sentinel.profile_name) {
        errors.push(format!("delete profile {}: {e}", sentinel.profile_name));
    }

    if let Err(e) = sentinel.delete_file() {
        errors.push(format!("delete sentinel: {e}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let detail = errors
            .iter()
            .map(|e| format!("  {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        Err(SandboxError::Cleanup(format!(
            "{} restore error(s):\n{detail}",
            errors.len()
        )))
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
