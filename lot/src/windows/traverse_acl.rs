#![allow(unsafe_code)]

//! Directory traverse ACE utilities for ALL APPLICATION PACKAGES.
//!
//! Grants and checks `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`
//! ACEs on directory ancestors so AppContainer sandboxed processes can walk
//! path components via `fs::metadata()`.

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    EqualSid, FreeSid, GetAce, GetAclInformation, PSECURITY_DESCRIPTOR, PSID,
};

use super::nul_device::allocate_app_packages_sid;
use crate::error::SandboxError;

/// ACCESS_ALLOWED_ACE_TYPE — not exported by windows-sys without extra features.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// Minimum rights for `fs::metadata()` to succeed on a directory.
const FILE_TRAVERSE: u32 = 0x0020;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const TRAVERSE_MASK: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

use super::{path_to_wide, win32_error_msg};

/// Walk parents of each path up to the volume root, deduplicate.
/// Does NOT include the paths themselves — only their ancestors.
pub fn compute_ancestors<P: AsRef<Path>>(paths: &[P]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for path in paths {
        // Canonicalize to resolve relative paths and symlinks.
        let Ok(canonical) = std::fs::canonicalize(path.as_ref()) else {
            continue;
        };
        let mut current = canonical.as_path();
        while let Some(parent) = current.parent() {
            // Stop when parent() returns the same path (volume root's parent is itself
            // on some implementations) or an empty path.
            if parent == current {
                break;
            }
            if seen.insert(parent.to_path_buf()) {
                result.push(parent.to_path_buf());
            }
            current = parent;
        }
    }

    result
}

/// Check if a directory's DACL has an allow ACE for ALL APPLICATION PACKAGES
/// with at least `TRAVERSE_MASK` (`FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`).
pub fn has_traverse_ace(path: &Path) -> bool {
    let wide = path_to_wide(path);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut dacl_ptr: *mut ACL = std::ptr::null_mut();

    // SAFETY: Reading the DACL. dacl_ptr points into sd's memory — only sd needs freeing.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
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
        return false;
    }

    let result = dacl_has_traverse_ace_for_app_packages(dacl_ptr);

    // SAFETY: sd allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd.cast());
    }

    result
}

/// Iterate ACEs in a DACL looking for an ACCESS_ALLOWED_ACE whose SID matches
/// ALL APPLICATION PACKAGES and whose mask includes `TRAVERSE_MASK`.
fn dacl_has_traverse_ace_for_app_packages(dacl: *mut ACL) -> bool {
    if dacl.is_null() {
        // NULL DACL = unrestricted access, traverse is implicitly granted.
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
        // SAFETY: SID from allocate_app_packages_sid.
        unsafe {
            FreeSid(app_sid);
        }
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

        // SAFETY: Both SIDs are valid — one from the ACE, one from AllocateAndInitializeSid.
        let sids_equal = unsafe { EqualSid(sid_ptr, app_sid) } != FALSE;
        if sids_equal && (ace.Mask & TRAVERSE_MASK) == TRAVERSE_MASK {
            found = true;
            break;
        }
    }

    // SAFETY: SID from allocate_app_packages_sid.
    unsafe {
        FreeSid(app_sid);
    }

    found
}

/// Grant `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES` with
/// `NO_INHERITANCE` for ALL APPLICATION PACKAGES on a single directory.
pub fn grant_traverse(path: &Path) -> crate::Result<()> {
    if has_traverse_ace(path) {
        return Ok(());
    }

    let wide = path_to_wide(path);
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading the current DACL of the directory.
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
            "failed to read DACL for {}: {}",
            path.display(),
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

    let result = apply_traverse_dacl(&wide, path, current_dacl, app_sid);

    // SAFETY: Freeing resources allocated above.
    unsafe {
        FreeSid(app_sid);
        LocalFree(sd.cast());
    }

    result
}

/// Build a new DACL with the traverse ACE and apply it to the directory.
fn apply_traverse_dacl(
    wide_path: &[u16],
    display_path: &Path,
    current_dacl: *mut ACL,
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
        grfAccessPermissions: TRAVERSE_MASK,
        grfAccessMode: GRANT_ACCESS,
        // Ancestors only — children are covered by policy path ACEs.
        grfInheritance: 0,
        Trustee: trustee,
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging a new ACE into the existing DACL.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, current_dacl, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build traverse DACL for {}: {}",
            display_path.display(),
            win32_error_msg(err),
        )));
    }

    // SAFETY: Applying the new DACL to the directory.
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
                "cannot modify DACL for {}: elevation (run as administrator) required",
                display_path.display(),
            )));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply traverse DACL for {}: {}",
            display_path.display(),
            win32_error_msg(err),
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_ancestors_empty_input() {
        let result = compute_ancestors::<&Path>(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn compute_ancestors_single_path() {
        // Use a path known to exist on all Windows machines.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = PathBuf::from(format!("{system_root}\\System32"));
        let ancestors = compute_ancestors(&[path.as_path()]);

        // Should contain at least the system root and the volume root.
        assert!(
            ancestors.len() >= 2,
            "expected at least 2 ancestors, got {}: {:?}",
            ancestors.len(),
            ancestors
        );

        // Should NOT contain the path itself.
        let canonical = std::fs::canonicalize(&path).ok();
        if let Some(ref c) = canonical {
            assert!(
                !ancestors.contains(c),
                "ancestors should not contain the input path itself"
            );
        }
    }

    #[test]
    fn compute_ancestors_deduplicates() {
        // Two paths under the same parent should not produce duplicate ancestors.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let p1 = PathBuf::from(format!("{system_root}\\System32"));
        let p2 = PathBuf::from(format!("{system_root}\\Temp"));
        let ancestors = compute_ancestors(&[p1.as_path(), p2.as_path()]);

        // Check no duplicates.
        let mut seen = std::collections::HashSet::new();
        for a in &ancestors {
            assert!(seen.insert(a), "duplicate ancestor: {}", a.display());
        }
    }
}
