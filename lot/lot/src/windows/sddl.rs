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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn test_tmp_base(name: &str) -> std::path::PathBuf {
        let ws_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let base = ws_root
            .join("test_tmp")
            .join(format!("{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("create test_tmp_base");
        base
    }

    #[test]
    fn get_sddl_system_directory() {
        // Reading SDDL from a known system directory should succeed.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let sddl = get_sddl(Path::new(&system_root)).expect("get_sddl should succeed");
        // SDDL strings start with "D:" for DACL.
        assert!(
            sddl.starts_with("D:"),
            "SDDL should start with 'D:': {sddl}"
        );
    }

    #[test]
    fn get_sddl_volume_root() {
        let sddl = get_sddl(Path::new(r"C:\")).expect("get_sddl on C:\\");
        assert!(!sddl.is_empty(), "SDDL should not be empty");
    }

    #[test]
    fn get_sddl_nonexistent_path_fails() {
        let result = get_sddl(Path::new(r"C:\NonExistent\Path\12345"));
        assert!(result.is_err(), "nonexistent path should produce an error");
    }

    #[test]
    fn get_sddl_restore_sddl_round_trip() {
        // Create a temp directory, read its SDDL, restore it, and verify.
        let base = test_tmp_base("sddl-roundtrip");

        let original_sddl = get_sddl(&base).expect("get_sddl on temp dir");
        assert!(!original_sddl.is_empty());

        // Restore the same SDDL back.
        restore_sddl(&base, &original_sddl).expect("restore_sddl should succeed");

        // Read again — SetNamedSecurityInfoW may apply auto-inheritance,
        // changing flags (e.g., D: → D:AI) and adding inherited ACEs.
        // Verify the restored SDDL is non-empty and starts with "D:".
        let restored_sddl = get_sddl(&base).expect("get_sddl after restore");
        assert!(
            restored_sddl.starts_with("D:"),
            "restored SDDL should start with 'D:': {restored_sddl}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_sddl_nonexistent_path_fails() {
        let result = restore_sddl(Path::new(r"C:\NonExistent\Path\12345"), "D:(A;;FA;;;WD)");
        assert!(result.is_err(), "nonexistent path should fail");
    }

    #[test]
    fn restore_sddl_invalid_sddl_string_fails() {
        let base = test_tmp_base("sddl-invalid");

        let result = restore_sddl(&base, "THIS_IS_NOT_VALID_SDDL");
        assert!(result.is_err(), "invalid SDDL string should fail");

        let _ = std::fs::remove_dir_all(&base);
    }
}
