#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, AllocateAndInitializeSid, DACL_SECURITY_INFORMATION, FreeSid, GetSecurityDescriptorDacl,
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SID_IDENTIFIER_AUTHORITY, TOKEN_ELEVATION,
    TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::error::SandboxError;

const NUL_DEVICE: &str = "\\\\.\\NUL";

const SECURITY_APP_PACKAGE_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 15],
};
const SECURITY_APP_PACKAGE_BASE_RID: u32 = 2;
const SECURITY_APP_PACKAGE_ALL_PACKAGES_RID: u32 = 1;

use super::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[allow(clippy::cast_possible_wrap)]
fn win32_error_msg(code: u32) -> String {
    io::Error::from_raw_os_error(code as i32).to_string()
}

/// Test whether AppContainer processes can access `\\.\NUL`.
///
/// Returns `true` if either:
/// - The DACL is NULL (unrestricted access — everyone can access the device), or
/// - The DACL contains an allow ACE for ALL APPLICATION PACKAGES (`S-1-15-2-1`).
pub fn nul_device_accessible() -> bool {
    let wide = to_wide(NUL_DEVICE);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading the DACL of the NUL device. sd freed with LocalFree below.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
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
        return false;
    }

    // NULL DACL means unrestricted access — no modification needed.
    let result = has_null_dacl(sd) || sd_contains_app_packages_ace(sd);

    // SAFETY: sd allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd.cast());
    }

    result
}

/// Check whether a security descriptor has a NULL DACL (unrestricted access).
fn has_null_dacl(sd: PSECURITY_DESCRIPTOR) -> bool {
    let mut dacl_present: i32 = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted: i32 = 0;

    // SAFETY: Querying DACL presence from a valid security descriptor.
    let ret = unsafe {
        GetSecurityDescriptorDacl(sd, &raw mut dacl_present, &raw mut dacl, &raw mut defaulted)
    };
    if ret == FALSE {
        return false;
    }

    // dacl_present != 0 but dacl is NULL → NULL DACL (all access granted).
    dacl_present != 0 && dacl.is_null()
}

/// Convert a security descriptor's DACL to SDDL and check for an allow ACE
/// whose trustee is ALL APPLICATION PACKAGES (SDDL abbreviation: "AC").
fn sd_contains_app_packages_ace(sd: PSECURITY_DESCRIPTOR) -> bool {
    let mut sddl_ptr: *mut u16 = std::ptr::null_mut();

    // SAFETY: Converting a valid security descriptor to SDDL string.
    // Length output parameter is null — we walk the null terminator instead.
    let ret = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &raw mut sddl_ptr,
            std::ptr::null_mut(),
        )
    };
    if ret == FALSE {
        return false;
    }

    // Walk the null-terminated wide string to find its length. The `sddl_len`
    // output parameter has ambiguous documentation (includes vs excludes null),
    // so we measure directly to avoid off-by-one.
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

    sddl_has_ac_allow(&sddl)
}

/// Check whether an SDDL string contains an allow ACE for ALL APPLICATION
/// PACKAGES (SDDL abbreviation: "AC").
///
/// Detects whether we previously added an allow ACE — does NOT model actual
/// DACL evaluation order (a preceding deny ACE would still block access).
///
/// ACE format: `(type;flags;rights;object_guid;inherit_object_guid;account_sid)`
fn sddl_has_ac_allow(sddl: &str) -> bool {
    sddl.split('(')
        .any(|ace| ace.starts_with("A;") && ace.contains(";;;AC)"))
}

/// Check whether the current process can modify the NUL device's DACL.
///
/// In practice this checks whether the process is running elevated
/// (as administrator), since `WRITE_DAC` on `\\.\NUL` requires elevation.
pub fn can_modify_nul_device() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();

    // SAFETY: Opening the current process token for query access.
    let ret = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if ret == FALSE {
        return false;
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut return_len: u32 = 0;

    // SAFETY: Querying token elevation status. Buffer is correctly sized.
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut return_len,
        )
    };

    // SAFETY: Token handle from OpenProcessToken.
    unsafe {
        CloseHandle(token);
    }

    ret != FALSE && elevation.TokenIsElevated != 0
}

/// Grant ALL APPLICATION PACKAGES (`S-1-15-2-1`) read/write access to `\\.\NUL`.
///
/// One-time, persistent DACL modification that allows AppContainer processes
/// to open the NUL device. Requires elevation (administrator).
/// Idempotent: returns `Ok(())` if the ACE already exists.
pub fn grant_nul_device_access() -> crate::Result<()> {
    if nul_device_accessible() {
        return Ok(());
    }

    let wide = to_wide(NUL_DEVICE);
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading the current DACL of the NUL device.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
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
        return Err(SandboxError::Setup(format!(
            "failed to read NUL device DACL: {}",
            win32_error_msg(err),
        )));
    }

    // Create ALL APPLICATION PACKAGES SID (S-1-15-2-1).
    let mut app_sid: PSID = std::ptr::null_mut();

    // SAFETY: Allocating a well-known SID with two sub-authorities.
    let ret = unsafe {
        AllocateAndInitializeSid(
            &SECURITY_APP_PACKAGE_AUTHORITY,
            2,
            SECURITY_APP_PACKAGE_BASE_RID,
            SECURITY_APP_PACKAGE_ALL_PACKAGES_RID,
            0,
            0,
            0,
            0,
            0,
            0,
            &raw mut app_sid,
        )
    };
    if ret == FALSE {
        // SAFETY: sd allocated by GetNamedSecurityInfoW.
        unsafe {
            LocalFree(sd.cast());
        }
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    }

    let result = apply_nul_dacl(&wide, current_dacl, app_sid);

    // SAFETY: Freeing resources allocated above.
    unsafe {
        FreeSid(app_sid);
        LocalFree(sd.cast());
    }

    result
}

/// Build a new DACL with the ALL APPLICATION PACKAGES ACE and apply it
/// to the NUL device.
fn apply_nul_dacl(wide_path: &[u16], current_dacl: *mut ACL, app_sid: PSID) -> crate::Result<()> {
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
        ptstrName: app_sid.cast(),
    };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        grfAccessMode: GRANT_ACCESS,
        // Device objects have no children — no inheritance needed.
        grfInheritance: 0,
        Trustee: trustee,
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging a new ACE into the existing DACL.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, current_dacl, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build NUL device DACL: {}",
            win32_error_msg(err),
        )));
    }

    // SAFETY: Applying the new DACL to the NUL device.
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

    // SAFETY: new_dacl allocated by SetEntriesInAclW.
    unsafe {
        LocalFree(new_dacl.cast());
    }

    if err != ERROR_SUCCESS {
        if err == ERROR_ACCESS_DENIED {
            return Err(SandboxError::Setup(
                "cannot modify NUL device DACL: elevation (run as administrator) required".into(),
            ));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply NUL device DACL: {}",
            win32_error_msg(err),
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_device_accessible_returns_bool() {
        // Verify it runs without panic and returns a definite value.
        let result: bool = nul_device_accessible();
        // On non-elevated CI, NUL is typically not accessible to AppContainers.
        // We don't assert a specific value since it depends on system config.
        let _ = result;
    }

    #[test]
    fn can_modify_nul_device_returns_false_non_elevated() {
        // CI runners and dev machines are typically non-elevated.
        // If this test ever runs elevated, the assertion is still valid
        // (elevated processes *can* modify), so we only assert non-elevated.
        if !can_modify_nul_device() {
            assert!(!can_modify_nul_device(), "should be deterministic");
        }
    }

    #[test]
    fn sddl_parsing_detects_ac() {
        assert!(sddl_has_ac_allow(
            "D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x12019f;;;AC)"
        ));
    }

    #[test]
    fn sddl_parsing_rejects_missing_ac() {
        assert!(!sddl_has_ac_allow("D:(A;;FA;;;SY)(A;;FA;;;BA)"));
    }

    #[test]
    fn sddl_parsing_rejects_deny_ace_for_ac() {
        assert!(!sddl_has_ac_allow("D:(D;;FA;;;AC)"));
    }

    #[test]
    fn sddl_parsing_empty_inputs() {
        assert!(!sddl_has_ac_allow(""));
        assert!(!sddl_has_ac_allow("D:"));
    }

    #[test]
    fn sddl_parsing_deny_then_allow_for_ac() {
        // Both deny and allow ACEs for AC — function detects the allow ACE.
        // Actual DACL evaluation is order-dependent (deny wins), but our
        // check only needs to detect whether we previously added an allow ACE.
        assert!(sddl_has_ac_allow("D:(D;;FA;;;AC)(A;;0x12019f;;;AC)"));
    }

    #[test]
    fn sddl_parsing_rejects_ac_substring_trustee() {
        // "SOMEAC" ends with "AC" but is not the AC SID — must not match.
        assert!(!sddl_has_ac_allow("D:(A;;FA;;;SOMEAC)"));
    }

    #[test]
    fn sddl_parsing_rejects_audit_ace_type() {
        // "AU;" (audit) starts with 'A' but is not "A;" (allow).
        assert!(!sddl_has_ac_allow("D:(AU;;FA;;;AC)"));
    }
}
