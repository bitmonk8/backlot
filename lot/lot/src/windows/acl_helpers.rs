#![allow(unsafe_code)]

//! Shared ACL manipulation helpers for NUL device, traverse ACE, and
//! AppContainer modules. Includes RAII wrappers for Win32 resources,
//! DACL read-modify-write primitives, and ACE-check utilities.

use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AllocateAndInitializeSid,
    DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetAce, GetAclInformation, PSECURITY_DESCRIPTOR,
    PSID, SID_IDENTIFIER_AUTHORITY,
};

use crate::error::SandboxError;

use super::win32_error_msg;

// ── Constants ────────────────────────────────────────────────────────

pub const SECURITY_APP_PACKAGE_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 15],
};
const SECURITY_APP_PACKAGE_BASE_RID: u32 = 2;
const SECURITY_APP_PACKAGE_ALL_PACKAGES_RID: u32 = 1;

/// ACCESS_ALLOWED_ACE_TYPE -- not exported by windows-sys without extra features.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// Marker substring embedded in `SandboxError::Setup` messages when the failure
/// is due to insufficient privilege (ACCESS_DENIED). Used by `appcontainer.rs`
/// and `traverse_acl.rs` to distinguish prerequisite failures from transient
/// I/O errors.
pub const ELEVATION_REQUIRED_MARKER: &str = "elevation required";

// ── RAII wrappers ────────────────────────────────────────────────────

/// RAII wrapper for a `PSID` allocated via `AllocateAndInitializeSid` or
/// `CreateAppContainerProfile`. Calls `FreeSid` on drop.
pub struct OwnedSid(PSID);

impl OwnedSid {
    /// Wrap a raw SID pointer. Returns `None` if null.
    pub const fn new(sid: PSID) -> Option<Self> {
        if sid.is_null() { None } else { Some(Self(sid)) }
    }

    pub const fn as_raw(&self) -> PSID {
        self.0
    }
}

impl Drop for OwnedSid {
    fn drop(&mut self) {
        // SAFETY: SID was validated non-null at construction and allocated by
        // a Win32 function that requires FreeSid for cleanup.
        unsafe {
            FreeSid(self.0);
        }
    }
}

/// RAII wrapper for a pointer allocated by a Win32 function that requires
/// `LocalFree` for cleanup (e.g., `GetNamedSecurityInfoW`, `SetEntriesInAclW`).
pub struct LocalFreeGuard<T>(*mut T);

impl<T> LocalFreeGuard<T> {
    /// Wrap a raw pointer. Returns `None` if null.
    pub const fn new(ptr: *mut T) -> Option<Self> {
        if ptr.is_null() { None } else { Some(Self(ptr)) }
    }

    pub const fn as_raw(&self) -> *mut T {
        self.0
    }
}

impl<T> Drop for LocalFreeGuard<T> {
    fn drop(&mut self) {
        // SAFETY: Pointer was validated non-null at construction and was
        // allocated by a Win32 function that requires LocalFree.
        unsafe {
            LocalFree(self.0.cast());
        }
    }
}

/// RAII wrapper for a `PSECURITY_DESCRIPTOR` from `GetNamedSecurityInfoW`.
pub type OwnedSecurityDescriptor = LocalFreeGuard<std::ffi::c_void>;

/// RAII wrapper for an ACL pointer from `SetEntriesInAclW`.
pub type OwnedAcl = LocalFreeGuard<ACL>;

// ── SID allocation ───────────────────────────────────────────────────

/// Allocate the ALL APPLICATION PACKAGES SID (`S-1-15-2-1`).
/// Returns an `OwnedSid` that frees the SID on drop.
pub fn allocate_app_packages_sid() -> Option<OwnedSid> {
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
    if ret == FALSE {
        None
    } else {
        OwnedSid::new(sid)
    }
}

// ── DACL read-modify-write primitives ────────────────────────────────

/// Low-level DACL modifier. Reads the current DACL of the named object,
/// merges the provided `EXPLICIT_ACCESS_W` entries, and applies the result.
///
/// `security_info_flags` controls which security information is written
/// (typically `DACL_SECURITY_INFORMATION`, optionally combined with
/// `PROTECTED_DACL_SECURITY_INFORMATION`).
///
/// Both `nul_device`, `traverse_acl`, and `appcontainer` build their own
/// `EXPLICIT_ACCESS_W` arrays and delegate to this function.
pub fn modify_dacl(
    wide_path: &[u16],
    display: &str,
    entries: &[EXPLICIT_ACCESS_W],
    security_info_flags: u32,
) -> crate::Result<()> {
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

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
    let _sd_guard = OwnedSecurityDescriptor::new(sd);

    merge_and_set_dacl(
        wide_path,
        display,
        current_dacl,
        entries,
        security_info_flags,
    )
}

/// Read the current DACL of a named object, merge a new ACE for the given SID,
/// and apply the updated DACL. Convenience wrapper around `modify_dacl` for
/// single-ACE GRANT_ACCESS operations.
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
    use windows_sys::Win32::Security::Authorization::{
        GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };

    let display = display_path.map_or_else(
        || String::from_utf16_lossy(wide_path.strip_suffix(&[0]).unwrap_or(wide_path)),
        |p| p.display().to_string(),
    );

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

    modify_dacl(wide_path, &display, &[ea], DACL_SECURITY_INFORMATION)
}

/// Build a new DACL from the entries and apply it to the object.
pub(super) fn merge_and_set_dacl(
    wide_path: &[u16],
    display: &str,
    current_dacl: *mut ACL,
    entries: &[EXPLICIT_ACCESS_W],
    security_info_flags: u32,
) -> crate::Result<()> {
    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging new ACEs into the existing DACL.
    #[allow(clippy::cast_possible_truncation)]
    let err = unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            current_dacl,
            &raw mut new_dacl,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build DACL for {display}: {}",
            win32_error_msg(err),
        )));
    }

    if new_dacl.is_null() {
        return Err(SandboxError::Setup(format!(
            "SetEntriesInAclW returned null DACL for {display}",
        )));
    }

    // SAFETY: null case handled above — OwnedAcl::new cannot return None.
    let Some(new_dacl_guard) = OwnedAcl::new(new_dacl) else {
        unreachable!("null DACL already checked above");
    };
    let dacl_ptr = new_dacl_guard.as_raw();

    // SAFETY: Applying the new DACL to the object.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            security_info_flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl_ptr,
            std::ptr::null(),
        )
    };

    drop(new_dacl_guard);

    if err != ERROR_SUCCESS {
        if err == ERROR_ACCESS_DENIED {
            return Err(SandboxError::Setup(format!(
                "cannot modify DACL for {display}: {ELEVATION_REQUIRED_MARKER} (run as administrator)",
            )));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply DACL for {display}: {}",
            win32_error_msg(err),
        )));
    }

    Ok(())
}

// ── ACE-check utilities ──────────────────────────────────────────────

/// Read the DACL and security descriptor for a named object.
/// Returns the DACL pointer and an owned security descriptor.
/// The DACL pointer points into the security descriptor memory.
pub fn read_dacl(wide_path: &[u16]) -> Option<(*mut ACL, OwnedSecurityDescriptor)> {
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut dacl_ptr: *mut ACL = std::ptr::null_mut();

    // SAFETY: Reading the DACL. dacl_ptr points into sd's memory.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut dacl_ptr,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }

    // sd must not be null for a successful call.
    let sd_guard = OwnedSecurityDescriptor::new(sd)?;
    Some((dacl_ptr, sd_guard))
}

/// Check if a DACL contains an ACCESS_ALLOWED ACE for ALL APPLICATION PACKAGES
/// whose mask includes all bits in `required_mask`.
///
/// A null DACL means unrestricted access (returns `true`).
pub fn dacl_has_app_packages_ace(dacl: *mut ACL, required_mask: u32) -> bool {
    if dacl.is_null() {
        // NULL DACL = unrestricted access, implicitly granted.
        return true;
    }

    let Some(app_sid) = allocate_app_packages_sid() else {
        return false;
    };

    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };

    // SAFETY: Querying ACL size info from a valid DACL.
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ret == FALSE {
        return false;
    }

    let mut found = false;
    for i in 0..info.AceCount {
        let mut ace_ptr: *mut std::ffi::c_void = std::ptr::null_mut();

        // SAFETY: Reading ACE at valid index from a valid DACL.
        let ret = unsafe { GetAce(dacl, i, &raw mut ace_ptr) };
        if ret == FALSE {
            continue;
        }

        // SAFETY: ace_ptr points to a valid ACE header within the DACL buffer.
        let ace = unsafe { &*(ace_ptr.cast::<ACCESS_ALLOWED_ACE>()) };
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }

        // The SID starts at SidStart field offset within the ACE struct.
        let sid_ptr: PSID = (&raw const ace.SidStart).cast_mut().cast();

        // SAFETY: Both SIDs are valid -- one from the ACE, one from AllocateAndInitializeSid.
        let sids_equal = unsafe { EqualSid(sid_ptr, app_sid.as_raw()) } != FALSE;
        if sids_equal && (ace.Mask & required_mask) == required_mask {
            found = true;
            break;
        }
    }

    found
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn dacl_has_app_packages_ace_round_trip() {
        let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
        let tmp = TempDir::new_in(&test_tmp).expect("create temp dir");

        let wide_path = super::super::path_to_wide(tmp.path());
        let app_sid = allocate_app_packages_sid().expect("allocate SID");

        let access_mask = super::super::FILE_GENERIC_READ | super::super::FILE_GENERIC_EXECUTE;

        apply_dacl(
            &wide_path,
            Some(tmp.path()),
            access_mask,
            0,
            app_sid.as_raw(),
        )
        .expect("apply_dacl");

        let (dacl, _sd) = read_dacl(&wide_path).expect("read_dacl");
        assert!(
            dacl_has_app_packages_ace(dacl, access_mask),
            "DACL should contain APP_PACKAGES ACE with the granted mask"
        );

        let write_mask = super::super::FILE_GENERIC_WRITE;
        assert!(
            !dacl_has_app_packages_ace(dacl, access_mask | write_mask),
            "DACL should NOT have write access"
        );
    }
}
