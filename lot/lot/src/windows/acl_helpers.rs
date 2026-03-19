#![allow(unsafe_code)]

//! Shared ACL manipulation helpers for NUL device and traverse ACE modules.

use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, AllocateAndInitializeSid, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SID_IDENTIFIER_AUTHORITY,
};

use crate::error::SandboxError;

use super::win32_error_msg;

pub const SECURITY_APP_PACKAGE_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 15],
};
const SECURITY_APP_PACKAGE_BASE_RID: u32 = 2;
const SECURITY_APP_PACKAGE_ALL_PACKAGES_RID: u32 = 1;

/// Allocate the ALL APPLICATION PACKAGES SID (`S-1-15-2-1`).
/// Caller must free with `FreeSid`.
pub fn allocate_app_packages_sid() -> Option<PSID> {
    let mut sid: PSID = std::ptr::null_mut();
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
            &raw mut sid,
        )
    };
    if ret == FALSE { None } else { Some(sid) }
}

/// Read the current DACL of a named object, merge a new ACE for the given SID,
/// and apply the updated DACL.
///
/// `wide_path`: null-terminated UTF-16 object name (for Win32 APIs).
/// `display_path`: human-readable path for error messages (None for device paths).
/// `access_mask`: desired access rights for the ACE.
/// `inheritance`: inheritance flags (0 for no inheritance).
pub fn apply_dacl(
    wide_path: &[u16],
    display_path: Option<&Path>,
    access_mask: u32,
    inheritance: u32,
    app_sid: PSID,
) -> crate::Result<()> {
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    let display = display_path.map_or_else(
        || String::from_utf16_lossy(wide_path.strip_suffix(&[0]).unwrap_or(wide_path)),
        |p| p.display().to_string(),
    );

    // SAFETY: Reading the current DACL.
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
        return Err(SandboxError::Setup(format!(
            "failed to read DACL for {display}: {}",
            win32_error_msg(err),
        )));
    }

    let result = merge_and_set_dacl(
        wide_path,
        &display,
        current_dacl,
        access_mask,
        inheritance,
        app_sid,
    );

    // SAFETY: sd allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd.cast());
    }

    result
}

/// Build a new DACL with the ACE and apply it to the object.
fn merge_and_set_dacl(
    wide_path: &[u16],
    display: &str,
    current_dacl: *mut ACL,
    access_mask: u32,
    inheritance: u32,
    app_sid: PSID,
) -> crate::Result<()> {
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
        ptstrName: app_sid.cast(),
    };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access_mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: inheritance,
        Trustee: trustee,
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging a new ACE into the existing DACL.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, current_dacl, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build DACL for {display}: {}",
            win32_error_msg(err),
        )));
    }

    // SAFETY: Applying the new DACL to the object.
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
            return Err(SandboxError::Setup(format!(
                "cannot modify DACL for {display}: elevation required (run as administrator)",
            )));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply DACL for {display}: {}",
            win32_error_msg(err),
        )));
    }

    Ok(())
}
