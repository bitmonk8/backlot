#![allow(unsafe_code)]

use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, AllocateAndInitializeSid, DACL_SECURITY_INFORMATION, FreeSid, GetSecurityDescriptorDacl,
    PSECURITY_DESCRIPTOR, PSID, SID_IDENTIFIER_AUTHORITY,
};

use crate::error::SandboxError;

const NUL_DEVICE: &str = "\\\\.\\NUL";

const SECURITY_APP_PACKAGE_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 15],
};
const SECURITY_APP_PACKAGE_BASE_RID: u32 = 2;
const SECURITY_APP_PACKAGE_ALL_PACKAGES_RID: u32 = 1;

use super::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};

use super::{to_wide, win32_error_msg};

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

// ---------------------------------------------------------------------------
// NUL device functions
// ---------------------------------------------------------------------------

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

    // dacl_present != 0 but dacl is NULL -> NULL DACL (all access granted).
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

pub use super::elevation::is_elevated;

/// Grant ALL APPLICATION PACKAGES (`S-1-15-2-1`) read/write access to `\\.\NUL`.
fn grant_nul_device() -> crate::Result<()> {
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

    let Some(app_sid) = allocate_app_packages_sid() else {
        // SAFETY: sd allocated by GetNamedSecurityInfoW.
        unsafe {
            LocalFree(sd.cast());
        }
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    };

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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// One-time elevated setup. Grants all ACEs needed for AppContainer sandboxes
/// to function correctly on Windows:
///   1. NUL device read/write for ALL APPLICATION PACKAGES
///   2. Traverse ACEs on each ancestor of the provided paths, up to (and
///      including) the volume root
///
/// Idempotent — safe to call multiple times. Requires elevation.
pub fn grant_appcontainer_prerequisites(paths: &[&Path]) -> crate::Result<()> {
    grant_nul_device()?;

    let ancestors = super::traverse_acl::compute_ancestors(paths);
    for ancestor in &ancestors {
        super::traverse_acl::grant_traverse(ancestor)?;
    }

    Ok(())
}

/// Checks whether all ancestors of each path (up to volume root) have the
/// ALL APPLICATION PACKAGES traverse ACE, and the NUL device ACE exists.
pub fn appcontainer_prerequisites_met(paths: &[&Path]) -> bool {
    if !nul_device_accessible() {
        return false;
    }

    let ancestors = super::traverse_acl::compute_ancestors(paths);
    ancestors
        .iter()
        .all(|a| super::traverse_acl::has_traverse_ace(a))
}

/// Checks prerequisites for all paths referenced by a [`SandboxPolicy`].
/// Delegates to [`appcontainer_prerequisites_met`] with the union of
/// `read_paths`, `write_paths`, and `exec_paths`.
pub fn appcontainer_prerequisites_met_for_policy(policy: &crate::policy::SandboxPolicy) -> bool {
    let paths = policy.all_paths();
    appcontainer_prerequisites_met(&paths)
}

/// Grants AppContainer prerequisites for all paths referenced by a
/// [`SandboxPolicy`]. Delegates to [`grant_appcontainer_prerequisites`]
/// with the union of `read_paths`, `write_paths`, and `exec_paths`.
pub fn grant_appcontainer_prerequisites_for_policy(
    policy: &crate::policy::SandboxPolicy,
) -> crate::Result<()> {
    let paths = policy.all_paths();
    grant_appcontainer_prerequisites(&paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_device_accessible_returns_bool() {
        // Verify it runs without panic and returns a definite value.
        let result: bool = nul_device_accessible();
        let _ = result;
    }

    #[test]
    fn is_elevated_returns_bool() {
        // CI runners and dev machines are typically non-elevated.
        if !is_elevated() {
            assert!(!is_elevated(), "should be deterministic");
        }
    }

    #[test]
    fn appcontainer_prerequisites_met_empty_paths() {
        // With no paths, only checks NUL device — must not panic.
        let result: bool = appcontainer_prerequisites_met(&[]);
        let _ = result;
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
