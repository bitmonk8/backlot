#![allow(unsafe_code)]

use std::io;
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, OBJECT_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

use super::{path_to_wide, to_wide};

#[allow(clippy::cast_possible_wrap)]
fn win32_to_io(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

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

    let result = sd_to_sddl(sd, DACL_SECURITY_INFORMATION);

    // SAFETY: sd was allocated by `GetNamedSecurityInfoW`.
    unsafe {
        LocalFree(sd.cast());
    }

    result
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
    // SetNamedSecurityInfoW re-evaluates inheritance from the parent, which
    // re-derives inherited ACEs to match the parent's current inheritable ACEs.
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
